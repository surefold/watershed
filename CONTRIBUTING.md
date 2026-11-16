# Contributing

Watershed is pre-release and its APIs still move. Issues and pull requests
are welcome; for anything larger than a fix, open an issue first so the
shape can be agreed before the work.

## Building and testing

Rust 1.85 or later.

```bash
cargo build --workspace
cargo test --workspace                           # no GPU or CUDA needed
cargo test -p watershed-gpu --features cuda      # GPU tests; needs the CUDA toolkit and an NVIDIA GPU
cargo run --example tour
cargo run --release --example benchmark

cd crates/watershed-py && uv sync && uv run pytest    # Python bindings
web/build.sh                                          # WebAssembly pages; needs Node 20+, see the grid README
npm run check                                         # TypeScript packages and pages, after web/build.sh
npx playwright install chromium && npm run e2e        # browser tests of the grid's pages, after web/build.sh
```

## Before a pull request

CI runs these, and a pull request should pass them:

```bash
cargo fmt --all --check
cargo clippy --locked --workspace --all-targets -- -D warnings
cargo test --locked --workspace
cargo doc --locked --workspace --no-deps
cargo build --locked --release --target wasm32-unknown-unknown -p watershed-web
web/build.sh && npm run check                    # the grid's TypeScript packages and pages
npm run e2e                                      # browser tests (web/e2e), after web/build.sh
```

CI has no GPU. If a change touches the GPU executor, run the `cuda` tests
on a machine that has one and say so in the pull request.

## What a change should come with

- **A test.** [docs/testing.md](docs/testing.md) says where each kind lives.
  Behaviour tests are named as sentences, and the list of names is the
  specification.
- **Agreement between the two ways of running.** A scenario plan must give
  what the graph gives; anything a plan learns to do needs a test against
  the graph.
- **A measurement, for a claim about speed.** [docs/benchmarks.md](docs/benchmarks.md)
  says how figures are taken and labelled: measured or scaled, how many
  runs, on what machine.
- **Documentation where the behaviour is described.** Each crate's README is
  its guide; [docs/limitations.md](docs/limitations.md) lists what is not
  done.

## Where things are

[docs/internals.md](docs/internals.md) describes the update algorithm, the
memory layout and the source layout. The [README](README.md#architecture)
lists the crates.

By contributing you agree that your work is licensed under the Apache
License 2.0, as the rest of the repository is.
