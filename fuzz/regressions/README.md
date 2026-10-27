Inputs that once crashed or hung a fuzz target, one folder per target,
named as libFuzzer wrote them (`crash-<sha1>`, `timeout-<sha1>`). CI copies them into the corpus
before each run, so a fix that comes undone fails the next run. Add the
same input to the target's unit tests too, where `cargo test` runs it on
every platform (for `term_feed`, `fuzz_regressions_do_not_panic` in
`crates/term/tests/vt.rs` reads this folder, so a new file there is enough).
