TODO:

- [ ] Explain the strange signature of `wait_for_and_map` (now non-existent) as a consequence of [this compiler issue](https://github.com/rust-lang/rust/issues/100013). No longer required, but keep this here for now in case this becomes an issue again.
- [ ] Complete documentation.
- [ ] Split `SensorWriter` functionality out into trait, for future exotic sensor writer types.
- [ ] Write no-alloc no-std single threaded async core.
- [ ] Improve benchmarking framework. Create benchmarks for both open and closed workloads.
- [ ] Add multithreaded async tests and investigate loom for robustness testing.
- [ ] Add blocking `Send` core.