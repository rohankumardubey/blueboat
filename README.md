# Blueboat

![CI](https://github.com/losfair/blueboat/actions/workflows/ci.yml/badge.svg)

Blueboat is an open-source alternative to Cloudflare Workers.

Blueboat aims to be a developer-friendly, multi-tenant platform for serverless web applications. A *monolithic* approach is followed: we try to implement features of commonly used libraries (in the web application context) natively in Rust to replace native Node addons, improve performance and reduce duplicated code. Blueboat's [architecture](#architecture) ensures the security of the platform, prevents code duplication and keeps the overhead low.

*If you think a JavaScript library should be natively re-implemented in Blueboat, feel free to open an issue or a pull request!*

A simple Blueboat application looks like:

```ts
Router.get("/", req => new Response("hello world"));

Router.get("/example", req => {
  return fetch("https://example.com");
});

Router.get("/yaml", req => {
  const res = TextUtil.Yaml.stringify({
    hello: "world",
  });
  return new Response(res);
});
```

## Quick start using the hosted service

**Warning: The hosted service is in Alpha stage and should only be used for testing purpose. There is absolutely no guarantee on uptime or data security for now.**

1. Install `bbcli`

For Linux:

```bash
curl -sSL -o /tmp/bbcli.tar.gz https://github.com/losfair/bbcli/releases/download/v0.1.0-alpha.1/bbcli_linux.tar.gz
tar -xzvf /tmp/bbcli.tar.gz -C ~
chmod +x ~/bbcli && rm /tmp/bbcli.tar.gz
```

For macOS:

```bash
curl -sSL -o /tmp/bbcli.tar.gz https://github.com/losfair/bbcli/releases/download/v0.1.0-alpha.1/bbcli_macos.tar.gz
tar -xzvf /tmp/bbcli.tar.gz -C ~
chmod +x ~/bbcli && rm /tmp/bbcli.tar.gz
```

2. Clone the example project

```bash
git clone https://github.com/losfair/blueboat-examples
```

3. Deploy the project

```bash
cd blueboat-examples/hello-world
npm i
~/bbcli deploy --vars ./hosted.vars.yaml
```

On the first `bbcli deploy` it should print out a link for logging in using your GitHub account.

Note the last line of the output. It should look like:

```
Key: managed/gh_XXXXXXX/com.example.blueboat.hello-world/metadata.json
```

The project is now available at `XXXXXXX-com--example--blueboat--hello-world.alpha.workers.rs`.

## Features

Supported and planned features:

- Standard JavaScript features supported by V8
- A subset of the Web platform API
  - [x] `fetch()`, `Request` and `Response` objects
  - [x] `TextEncoder`, `TextDecoder`
  - [ ] Timers
    - [x] `setTimeout`, `clearTimeout`
    - [ ] `setInterval`, `clearInterval`
  - [x] `URL`, `URLSearchParams`
  - [x] `crypto.getRandomValues`
  - [ ] `crypto.subtle`
  - [ ] `console`
    - [x] `console.log()`
    - [x] `console.warn()`
    - [x] `console.error()`
- Request router
  - [x] The `Router` object
- Cryptography extensions
  - [x] Ed25519 and X25519: `NativeCrypto.Ed25519`, `NativeCrypto.X25519`
  - [x] JWT signing and verification: `NativeCrypto.JWT`
  - [x] Hashing: `NativeCrypto.digest`
- Graphics API
  - [x] Canvas (`Graphics.Canvas`)
  - [x] Layout constraint solver based on Z3 (`Graphics.Layout`)
- Template API
  - [x] [Tera](https://github.com/Keats/tera) template rendering: `Template.render()`
- Encoding and decoding
  - [x] `Codec.hexencode()`, `Codec.hexdecode()`
  - [x] `Codec.b64encode()`, `Codec.b64decode()`
- Embedded datasets
  - [x] MIME type guessing: `Dataset.Mime.guessByExt()`
- Background tasks
  - [x] `Background.atMostOnce()`
  - [ ] `Background.atLeastOnce()`
- Data validation
  - [x] JSON Type Definition validation: `Validation.JTD`
- Text utilities
  - [x] YAML serialization and deserialization: `TextUtil.Yaml.parse()`, `TextUtil.Yaml.stringify()`
  - [x] Markdown rendering: `TextUtil.Markdown.renderToHtml()`
- Native API to external services
  - [x] MySQL client
  - [x] Apple Push Notification Service (APNS) client

The entire API definition is published as the [blueboat-types](https://www.npmjs.com/package/blueboat-types) package.

## Deploy your own Blueboat instance

### Prerequisites

- [Docker](https://www.docker.com/)
- An S3-compatible bucket for storing application configuration and code
- A MySQL service for storing [bbcp](https://github.com/losfair/bbcp) metadata
- (Optional) A Kafka service for streaming logs

### Deploy

Example docker-compose file:

```yaml
version: "3"
services:
  blueboat:
    image: ghcr.io/losfair/blueboat:latest
    user: daemon
    ports:
    - "127.0.0.1:3000:3000"
    entrypoint:
    - /usr/bin/blueboat_server
    - -l
    - 0.0.0.0:3000
    - --s3-bucket
    - my-bucket.example.com
    - --s3-region
    - us-east-1
    # Uncomment this if you use a non-AWS S3-compatible service.
    # - --s3-endpoint
    # - https://minio.example.com
    # Uncomment this to enable logging to Kafka.
    # - --log-kafka
    # - net.univalent.blueboat-log.default:0@kafka:9092
    # Uncomment this to enable geoip information in the `x-blueboat-client-country`,
    # `x-blueboat-client-city`, `x-blueboat-client-subdivision-1` and
    # `x-blueboat-client-subdivision-2` request headers.
    # - --mmdb-city
    # - /opt/blueboat/mmdb/GeoLite2-City.mmdb
    # Uncomment this to enable automatic Wikipedia IP blocklist query in the
    # `x-blueboat-client-wpbl` request header.
    # The database is generated with https://github.com/losfair/wpblsync.
    # - --wpbl-db
    # - /opt/blueboat/wpbl/wpbl.db
    environment:
      RUST_LOG: info
      AWS_ACCESS_KEY_ID: your_s3_access_key_id
      AWS_SECRET_ACCESS_KEY: your_s3_secret_access_key
      # Uncomment this to enable text rendering in canvas.
      # SMRAPP_BLUEBOAT_FONT_DIR: /opt/blueboat/fonts
```

### Set up a reverse proxy

Blueboat loads the application's metadata from the S3 key specified in the `X-Blueboat-Metadata` header. This information should be provided by a reverse proxy such as Nginx or Caddy.

I run my Blueboat instances behind [Caddy](https://caddyserver.com/). An example config looks like:

```
hello.blueboat.example.com {
  reverse_proxy blueboat:3000 {
    # S3 path to application metadata
    header_up X-Blueboat-Metadata "test/metadata.json"
    header_up X-Blueboat-Client-Ip "{remote_host}"

    # Prevent faked request id
    header_up -X-Blueboat-Request-Id
  }
}
```

### Deploy bbcp

[bbcp](https://github.com/losfair/bbcp) is the service that manages application deployment on Blueboat, and is itself a Blueboat application. We need to manually bootstrap it:

1. Install [s3cmd](https://github.com/s3tools/s3cmd) and [zx](https://github.com/google/zx).

2. Set up configuration:

```bash
git clone https://github.com/losfair/bbcp
mkdir bbcp-config
cd bbcp-config
cat > env.json << EOF
{
  "s3AccessKeyId": "your_s3_access_key_id",
  "s3SecretAccessKey": "your_s3_secret_access_key",
  "s3Region": "us-east-1",
  "s3Bucket": "your-bucket.example.com",
  "s3Prefix": "managed/",
  "ghClientId": "your-github-client-id",
  "ghClientSecret": "your-github-client-secret"
}
EOF
cat > mysql.json << EOF
{
  "db": {
    "url": "mysql://user:password@mysql-server/database"
  }
}
EOF
cat > s3.config << EOF
access_key = your_s3_access_key_id
secret_key = your_s3_secret_access_key
use_https = True
check_ssl_certificate = True
EOF
cat > run_deploy.sh << EOF
#!/bin/bash

set -euo pipefail
cd "$(dirname $0)"
cd ../bbcp/

CONFIG_PATH=../bbcp-config S3_BUCKET=your-bucket.example.com ./deploy.sh
EOF
chmod +x run_deploy.sh
./run_deploy.sh
```

Next, follow the [set up a reverse proxy](#set-up-a-reverse-proxy) section to set up a public endpoint for your bbcp application. And done! You can now use `bbcli` to deploy to your Blueboat instance.

## Architecture

Blueboat before the rewrite ([commit](https://github.com/losfair/blueboat/commit/eba320fd8c4806fc39b764a5a2368a0c6c34d976)) used the same execution model as Cloudflare Workers where multiple applications run in different isolates in a single process. This of course has the benefit of reduced execution overhead, but poses several problems:

- Security

With isolates sharing the same process, if there is a single exploitable memory safety bug in any dependency of the engine, all other tenants' apps running in the same process is compromised.

This requires the engine developers to be extremely conservative when introducing new features and dependencies. Blueboat has built-in support for MySQL, Canvas, APNS push notification and constraint solving with Z3 (all added because I found them useful) and I'm not very confident that there is not a single memory safety vulnerability in all these complex dependencies.

- Operational cost

Cloudflare has their team that operates the Workers platform, while most users who want to self-host their infrastructure don't. So this leaves a long [patch gap](https://developers.cloudflare.com/workers/learning/security-model#v8-bugs-and-the-patch-gap) after a vulnerability is discovered, during which the engine is vulnerable.

For all these reasons I decided to reconsider the execution model. Blueboat after the rewrite uses a tightly-coupled multi-process architecture based on [smr](https://github.com/losfair/smr) where each application runs in its own set of forked processes and isolated with [seccomp](https://man7.org/linux/man-pages/man2/seccomp.2.html). The cold-start overhead is higher than isolates in a shared process (in the tens of milliseconds range), but still far lower than a full container running node.

The APIs as described in [features](#features) are mostly implemented in Blueboat's native code which is shared between forked processes, so there isn't N copies for N application instances - there is only one copy of the native code. This allows Blueboat to follow a monolithic approach without incurring significant overhead, where a lot of features are directly bundled into the engine instead of requiring the user to pull in their own dependencies.

## Web apps built on Blueboat

Blueboat currently powers several services running on `.univalent.net`, `.invariant.cn` and `.palette.cat`. Public ones include:

- [https://id.invariant.cn](https://id.invariant.cn)
- [https://lambda.app.invariant.cn](https://lambda.app.invariant.cn)
