use std::{cell::RefCell, collections::HashMap, time::Duration};

use crate::{
  api::{
    util::{mk_v8_string, v8_serialize, write_applog},
    API,
  },
  app_mysql::AppMysql,
  bootstrap::{BlueboatBootstrapData, JSLAND_SNAPSHOT},
  exec::Executor,
  lpch::LowPriorityMsg,
  metadata::{ApnsEndpointMetadata, Metadata},
  package::{Package, PackageKey},
  registry::SymbolRegistry,
  reliable_channel::{ReliableChannel, ReliableChannelSeed},
  v8util::{IsolateInitDataExt, ObjectExt},
};
use anyhow::{bail, Result};
use parking_lot::Mutex;
use serde::{Deserialize, Serialize};
use smr::{
  ipc_channel::ipc::{IpcSender, IpcSharedMemory},
  types::InitData,
};
use std::convert::TryFrom;
use thiserror::Error;
use tokio::runtime::Handle;
use v8;

pub const NI_ENTRY_KEY: &str = "__blueboat_host_invoke";

#[derive(Serialize, Deserialize)]
pub struct BlueboatInitData {
  pub key: PackageKey,
  pub package: IpcSharedMemory,
  pub metadata: Metadata,
  pub lp_tx: IpcSender<LowPriorityMsg>,
  pub rch: Option<ReliableChannelSeed>,
}

impl InitData for BlueboatInitData {
  fn process_name(&self) -> String {
    format!("{}", self.key)
  }
}

pub struct BlueboatCtx {
  pub key: &'static PackageKey,
  pub package: Package,
  pub metadata: &'static Metadata,
  pub lp_tx: &'static IpcSender<LowPriorityMsg>,
  pub rch: ReliableChannel,
  pub isolate: Mutex<v8::OwnedIsolate>,
  pub v8_ctx: RefCell<v8::Global<v8::Context>>,
  pub context_template: v8::Global<v8::ObjectTemplate>,
  pub http_client: reqwest::Client,
  pub mysql: HashMap<String, AppMysql>,
  pub apns: HashMap<String, a2::Client>,
  pub computation_watcher: Handle,
}

impl BlueboatCtx {
  pub fn init(mut d: BlueboatInitData) -> &'static Self {
    let rch = d.rch.take().unwrap().run_forever();
    let d: &'static BlueboatInitData = Box::leak(Box::new(d));
    let mut isolate = v8::Isolate::new(v8::CreateParams::default().snapshot_blob(JSLAND_SNAPSHOT));
    isolate.set_slot(SymbolRegistry::new());
    isolate.set_slot(d);
    let package = Package::load(&d.package[..]);
    isolate.set_microtasks_policy(v8::MicrotasksPolicy::Auto);
    let context_template = Self::build_context_template(&mut isolate);

    let computation_watcher_rt = tokio::runtime::Builder::new_current_thread()
      .enable_time()
      .build()
      .unwrap();
    let computation_watcher = computation_watcher_rt.handle().clone();
    std::thread::spawn(move || {
      computation_watcher_rt.block_on(futures::future::pending::<()>());
    });

    let app_key = &d.key;
    let init_timeout_watcher = computation_watcher.spawn(async move {
      tokio::time::sleep(Duration::from_secs(3)).await;
      log::error!("app {}: initialization timed out", app_key);
      std::process::exit(1);
    });
    let v8_ctx;
    {
      let scope = &mut v8::HandleScope::new(&mut isolate);
      match Self::build_v8_context(scope, &context_template, &package, &d.metadata) {
        Ok(x) => {
          v8_ctx = x;
        }
        Err(e) => {
          write_applog(scope, format!("failed to build v8 context: {}", e));
          log::debug!("app {}: failed to build v8 context: {:?}", app_key, e);
          std::process::exit(1);
        }
      }
    }
    init_timeout_watcher.abort();

    let me = Self {
      key: &d.key,
      package,
      metadata: &d.metadata,
      lp_tx: &d.lp_tx,
      rch,
      isolate: Mutex::new(isolate),
      v8_ctx: RefCell::new(v8_ctx),
      context_template,
      http_client: reqwest::Client::new(),
      mysql: d
        .metadata
        .mysql
        .iter()
        .filter_map(|(k, v)| {
          mysql_async::Pool::from_url(&v.url)
            .ok()
            .map(|x| (k.clone(), AppMysql::new(x)))
        })
        .collect(),
      apns: d
        .metadata
        .apns
        .iter()
        .filter_map(|(k, v)| {
          a2::Client::certificate(
            &mut &v.cert[..],
            "",
            match v.endpoint {
              ApnsEndpointMetadata::Production => a2::Endpoint::Production,
              ApnsEndpointMetadata::Sandbox => a2::Endpoint::Sandbox,
            },
          )
          .ok()
          .map(|x| (k.clone(), x))
        })
        .collect(),
      computation_watcher,
    };
    let me: &'static BlueboatCtx = Box::leak(Box::new(me));
    me
  }

  fn build_context_template(isolate: &mut v8::Isolate) -> v8::Global<v8::ObjectTemplate> {
    let scope = &mut v8::HandleScope::new(isolate);
    let obj_t = v8::ObjectTemplate::new(scope);

    let blueboat_host_invoke = v8::FunctionTemplate::new(scope, native_invoke_entry);
    obj_t.set(
      v8::String::new(scope, NI_ENTRY_KEY).unwrap().into(),
      blueboat_host_invoke.into(),
    );
    v8::Global::new(scope, obj_t)
  }

  pub fn grab_v8_context<'s>(&self) -> v8::Global<v8::Context> {
    (*self.v8_ctx.borrow()).clone()
  }

  pub fn reset_v8_context<'s>(&self, scope: &mut v8::HandleScope<'s, ()>) {
    SymbolRegistry::current(scope).clear();
    let ctx = Self::build_v8_context(scope, &self.context_template, &self.package, self.metadata)
      .expect("reset failed");
    *self.v8_ctx.borrow_mut() = ctx;
  }

  fn build_v8_context<'s>(
    scope: &mut v8::HandleScope<'s, ()>,
    template: &v8::Global<v8::ObjectTemplate>,
    package: &Package,
    md: &Metadata,
  ) -> Result<v8::Global<v8::Context>> {
    #[derive(Error, Debug)]
    #[error("package init error: {0}")]
    pub struct PackageInitError(String);

    let template = v8::Local::new(scope, template);
    let ctx = v8::Context::new_from_template(scope, template);
    {
      let scope = &mut v8::ContextScope::new(scope, ctx);

      // Load package contents.
      let pack = package.pack(scope);
      ctx.global(scope).set_ext(scope, "Package", pack);
      let version_string = mk_v8_string(scope, env!("CARGO_PKG_VERSION"))?;
      ctx
        .global(scope)
        .set_ext(scope, "__blueboat_version", version_string.into());

      if let Ok(analytics_domain) = std::env::var("SMRAPP_BLUEBOAT_ANALYTICS_URL") {
        let s = mk_v8_string(scope, &analytics_domain)?;
        ctx
          .global(scope)
          .set_ext(scope, "__blueboat_env_analytics_url", s.into());
      }

      // Bootstrap.
      let bootstrap_data = BlueboatBootstrapData {
        mysql: md.mysql.keys().cloned().collect(),
        apns: md.apns.keys().cloned().collect(),
        env: md.env.clone(),
      };
      let bootstrap_data = v8_serialize(scope, &bootstrap_data)?;
      {
        let catch = &mut v8::TryCatch::new(scope);
        let bootstrap = ctx.global(catch).get_ext(catch, "__blueboat_app_bootstrap");
        if let Ok(f) = v8::Local::<v8::Function>::try_from(bootstrap) {
          let undef = v8::undefined(catch);
          f.call(catch, undef.into(), &[bootstrap_data]);
          if let Some(exc) = catch.exception() {
            return Err(PackageInitError(exc.to_rust_string_lossy(catch)).into());
          }
        }
      }

      let index = package.load_module_with_dependencies(scope, "");
      if let Some(index) = index {
        let _ = index.evaluate(scope);
        if matches!(index.get_status(), v8::ModuleStatus::Errored) {
          let exc = index.get_exception();
          let stack = v8::Local::<v8::Object>::try_from(exc)
            .ok()
            .map(|x| x.get_ext(scope, "stack").to_rust_string_lossy(scope))
            .unwrap_or_default();
          return Err(PackageInitError(stack).into());
        }
      }
    }
    Ok(v8::Global::new(scope, ctx))
  }
}

pub fn native_invoke_entry(
  scope: &mut v8::HandleScope,
  args: v8::FunctionCallbackArguments,
  retval: v8::ReturnValue,
) {
  let res = native_invoke_entry_impl(scope, args, retval);
  if let Err(e) = res {
    log::error!("{}", e);
    let msg = v8::String::new(scope, &format!("{}", e)).unwrap();
    let exc = v8::Exception::error(scope, msg);
    scope.throw_exception(exc);
  }
}

fn native_invoke_entry_impl<'s>(
  scope: &mut v8::HandleScope,
  args: v8::FunctionCallbackArguments,
  retval: v8::ReturnValue,
) -> Result<()> {
  let package_key = &scope.get_init_data().key;
  let request_id = Executor::try_current()
    .and_then(|x| x.upgrade())
    .map(|x| x.request_id.clone())
    .unwrap_or_else(|| "unknown".to_string());

  let api_name = v8::Local::<v8::String>::try_from(args.get(0))
    .map_err(|e| {
      anyhow::anyhow!(
        "cannot get native api name, app {} request {:?}: {}",
        package_key,
        request_id,
        e
      )
    })?
    .to_rust_string_lossy(scope);
  log::debug!("native invoke: {}", api_name);
  if let Some(f) = API.get(&api_name) {
    if let Err(e) = f(scope, args, retval) {
      bail!(
        "native invoke error from app {} request {:?}: {}",
        package_key,
        request_id,
        e
      );
    }
    Ok(())
  } else {
    bail!(
      "app {} request {:?} is invoking an unknown native api: {}",
      package_key,
      request_id,
      api_name
    );
  }
}
