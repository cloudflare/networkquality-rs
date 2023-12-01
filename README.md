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

Or install it with cargo:
```shell
cargo install --path ./cli

# run an RPM test
mach
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

> By default this measures responsiveness against Cloudflare's responsiveness
server.

Running a responsiveness test with Apple's server: 
```shell
mach rpm -c https://mensura.cdn-apple.com/.well-known/nq
```

Timing the download of a resource:
```shell
mach download https://cloudflare.com/cdn-cgi/trace
 time_lookup: 0.0000
time_connect: 0.0531
 time_secure: 0.1144
    duration: 0.2224

         bps: 8130.7393
 bytes_total: 226
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

The main complexity in the repo is due to the `Network` and `Time` trait
abstractions. We need those abstractions for two reasons. First, it allows us to
abstract over the underlying request/response implementation which will help
with WASM/browser support in the future. Second, we use the network abstraction
to define a Proxy network. This allows us to not only directly test H1, H2, or
H3 connections, but also to test the impact of a multi-hop proxy network. The
network trait is built in composable manner; multiple proxy networks can be
layered on each other to build any proxy configuration.

## Crates

networkquality-rs is split into multiple crates are under the `./crates`
directory. They are combined together to form the `mach` cli under `./cli`.

- `nq-core`: the core crate which defines the `Time` and `Network` abstractions.
  It also provides functionality for creating and driving H1 and H2 connections
  over an abstracted IO transport: `ByteStream`. Finally, it provides a
  low-level HTTP `Client` used to send arbitrary HTTP requests as well as a
  `ThroughputClient` which uses a special `CountingBody` to measure the
  throughput of an HTTP connection. Finally, it provides a very simple
  `Speedtest` trait used for running and reporting the results of a test.

- `nq-tokio-network`: a `Network` implementation based on tokio (and indirectly
  on hyper).)

- `nq-proxy-network`: a `Network` implementation that wraps another network. To
  create a new connection for users, the `ProxyNetwork` sends `CONNECT` requests
  over the inner, wrapped `Network` to the configured proxy server. This is
  currently unused, however will be added in the future.

- `nq-stats`: provides `Timeseries` and `Counter` types for storing measurements
  and running simple statisitcs on those series.

- `nq-rpm`: a speedtest which implements the ["Responsiveness under Working
  Conditions"](draft) draft.

- `nq-latency`: a speedtest which measures latency by timing how long it takes
  to setup multiple TCP connections.

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
- [ ] RPM stability decreases as interval duration decreases. Look into calculating
  a better `CountingBody` update rate.
- [x] Properly signal the connections on a network to shutdown.

[draft]: https://datatracker.ietf.org/doc/html/draft-ietf-ippm-responsiveness-03