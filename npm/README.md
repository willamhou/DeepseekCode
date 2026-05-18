# @deepseek-code/cli

This package is the npm command wrapper for the `deepseek` CLI.

Published installs resolve the binary from the platform-specific optional npm
package for the current host:

- `@deepseek-code/cli-linux-x64`
- `@deepseek-code/cli-macos-x64`
- `@deepseek-code/cli-macos-arm64`
- `@deepseek-code/cli-windows-x64`

Release builds can also place platform binaries under these legacy wrapper paths:

```text
npm/bin/<target-triple>/deepseek
npm/bin/<target-triple>/deepseek.exe
```

Supported target-triple directory names used by the wrapper:

- `x86_64-unknown-linux-gnu`
- `aarch64-unknown-linux-gnu`
- `x86_64-apple-darwin`
- `aarch64-apple-darwin`
- `x86_64-pc-windows-msvc`
- `aarch64-pc-windows-msvc`

For local testing, set `DEEPSEEK_BINARY` to an existing binary:

```bash
DEEPSEEK_BINARY=../target/release/deepseek node bin/deepseek.js version
DEEPSEEK_BINARY=../target/release/deepseek npm run test:tui-entrypoint
```

The release matrix stages each compiled binary into `npm/platforms/<platform>/bin`
and packs the matching platform npm tarball before publishing the root wrapper.
