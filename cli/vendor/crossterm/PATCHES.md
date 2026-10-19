# Difu's Crossterm backport

Source: the crates.io `crossterm` **0.29.0** release, SHA-256
`d8b9f2e4c67f833b660cdb0a3523065869fb35570177239812ed4c905aeff87b`.
The upstream MIT license is included in `LICENSE`.

This copy preserves the released source, features, and runtime dependencies.
`Cargo.toml` is the release's `Cargo.toml.orig` without upstream development
dependencies and example targets; those assets are not vendored.
Release CRLF line endings are normalized to LF for this repository; trailing
whitespace in the release README and manifest is removed.

Two synchronous input fixes are backported from the approach in
[upstream PR #1067](https://github.com/crossterm-rs/crossterm/pull/1067):

- `src/event/source/unix/mio.rs`: return `UnexpectedEof` on zero-byte reads,
  propagate fatal read errors, and retain the existing `WouldBlock`/`Interrupted`
  handling. This lets difu's error path restore the terminal and shut down.
- `src/cursor/sys/unix.rs`: propagate poll errors rather than retrying forever
  while requesting the terminal cursor position.

`src/terminal/sys/unix.rs` also drops redundant parentheses flagged by the
current compiler; this has no behavior change.

See [upstream issue #793](https://github.com/crossterm-rs/crossterm/issues/793).
Difu uses the synchronous mio backend; this backport does not fix the optional
`use-dev-tty` or `event-stream` backends, which difu does not enable.

`tests/terminal_disconnect.rs` exercises real pseudo-terminal closure, with and
without a hangup signal, and preserves normal Ctrl+C input. Remove this patch
and directory once a compatible upstream release fixes these paths and passes
the regression tests. Do not replace this with a patch to the local Cargo cache.
