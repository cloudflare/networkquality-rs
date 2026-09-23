# networkquality-rs

networkquality-rs is a collection of tools for measuring the quality of a
network. This repo provides a CLI tool `mach` which can be used to run multiple
different tests. The main focus of `mach` and this repo is to implement the IETF
draft: ["Responsiveness under Working Conditions"](draft).

The draft defines "responsiveness", measured in **R**ound trips **P**er
**M**inute (RPM), as a useful measurement of network quality. `mach`'s default
operation is to measure the responsiveness of a network using Cloudflare's
responsiveness servers.

# Installing

First, [install rust](https://www.rust-lang.org/tools/install).

Then build and run the binary at `./target/release/mach`:

```shell
cargo build --release

# run an rpm test
./target/release/mach
```

# Running `mach`

`mach` defaults to running a responsiveness test when given no arguments, the
equivalent to `mach rpm`.

Use `mach help` to see a list of subcommands and `mach help <subcommand>` or
`mach <subcommand> help` to see options for that command.

## Examples

Running a responsiveness test:

```shell
mach rpm
{
  "unloaded_latency_ms": 10.819,
  "jitter_ms": 6.945,
  "download": {
    "throughput": 104846062,
    "loaded_latency_ms": 86.936,
    "rpm": 446
  },
  "upload": {
    "throughput": 48758784,
    "loaded_latency_ms": 206.837,
    "rpm": 433
  }
}
```

> RPM reports are automatically uploaded to Cloudflare's aim database and are
> anonymous. See https://blog.cloudflare.com/aim-database-for-internet-quality/
> for more information.
>
> Use `--disable-aim-scores` to disable uploading reports.

Running a responsiveness test with Apple's server:

```shell
mach rpm -c https://mensura.cdn-apple.com/.well-known/nq
```

Timing the download of a resource:

```shell
mach download https://cloudflare.com/cdn-cgi/trace
{
  "dns_time": 0.0,
  "time_connect": 0.078,
  "time_secure": 0.243,
  "time_body": 0.0,
  "time_total": 0.243,
  "bytes_total": 228,
  "throughput": 7476
}
```

Measuring latency using TCP connection timing:

```shell
mach rtt
{
  "jitter_ms": 2.949,
  "latency_ms": 10.549
}
```

## Debugging

`mach` respects `RUST_LOG` env variables. If you want (likely too much)
information on what's happening internally, run mach with `RUST_LOG=info` set.

```shell
RUST_LOG=info mach
```

# Architecture

`mach` is distributed as the single, binary-only Cargo package `cf-mach`. Its
implementation is split into private modules under `./src`; they are not separate
packages or public Rust APIs.

The main complexity comes from the `Network` and `Time` trait abstractions. They
decouple measurements from the request/response and clock implementations, support
deterministic tests, and leave room for future WASM/browser transports. The network
abstraction is composable so alternative transports can be added without changing
the measurement algorithms.

The main modules are:

- `nq_core`: time and network abstractions, HTTP/1 and HTTP/2 connections, low-level
  clients, request bodies, and throughput accounting.
- `nq_tokio_network`: the Tokio-backed `Network` implementation.
- `nq_stats`: time-series and counter statistics used by measurements.
- `nq_latency`: TCP and HTTP latency measurement.
- `nq_load_generator`: sustained upload and download load generation.
- `nq_rpm`: responsiveness-under-working-conditions measurement.
- `nq_packetloss`: WebRTC/TURN packet-loss measurement.
- The remaining modules implement CLI arguments, commands, reporting, and AIM score
  submission.

# Releasing

Releases are cut from `main` by GitHub Actions. The version in `Cargo.toml` is the source of truth, and every release publishes prebuilt `mach` binaries to [GitHub Releases](https://github.com/cloudflare/networkquality-rs/releases).

1. In the Actions tab, run the **cut release** workflow and pick `patch`, `minor`, or `major`. It bumps the version in `Cargo.toml` and `Cargo.lock` on a `release/vX.Y.Z` branch and opens a `release: vX.Y.Z` pull request into `main`.
2. Review and merge that pull request.
3. The **tag release** workflow tags the merge commit `vX.Y.Z` and builds `mach` for Linux (x86_64), macOS (arm64), and Windows (x86_64).

# TODOs

- [ ] implement upload command.
  - [ ] uploading a given number of bytes.
  - [ ] uploading arbitrary files.
- [x] time DNS resolution.
- [ ] better TUI experience for all commands.
- [ ] QUIC support.
- [ ] MASQUE proxying support.
  - [ ] support RPK TLS.
- [ ] Output format:
  - [x] JSON
    - [ ] determine stability / extensions.
  - [ ] Human output
- [x] send AIM score reports
- [ ] automated testing
  - [ ] latency comparisions with curl
  - [ ] RPM comparisions with different tools against the same server
  - [ ] review/better test statistics
- [ ] socket stats for measuring connection throughput
- [ ] RPM stability decreases as interval duration decreases. Look into
      calculating a better `CountingBody` update rate.
- [x] Properly signal the connections on a network to shutdown.

[draft]: https://datatracker.ietf.org/doc/html/draft-ietf-ippm-responsiveness-03
