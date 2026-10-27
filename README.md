# Rostr

**AI Model Resource Registry**

Rostr is a lightweight CLI for AI agents to discover, evaluate, and select AI
models based on capabilities, benchmarks, available quotas, and availability.

The project aims to unify model metadata, evaluation results, and subscription
quotas. Queries and selection should use metadata and deterministic algorithms
without consuming model tokens. Rostr does not perform inference, proxy model
requests, or route traffic.

## Status

Version 0.1.0 is an initial CLI skeleton. It displays help and version information;
model discovery, evaluation, selection, and registry data are not implemented yet.

## Install

```sh
cargo install rostr
```

## Usage

```sh
rostr
rostr --help
rostr --version
```

Running without arguments displays help. Short options `-h` and `-V` are also
supported.

## Development

```sh
cargo check
cargo fmt --check
```

Source: [github.com/kookyleo/rostr](https://github.com/kookyleo/rostr)

## License

Licensed under [Apache-2.0](LICENSE).
