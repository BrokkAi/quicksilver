# cargo-quicksilver

Impact-selected mutation testing for Cargo workspaces. A mutant only runs the
tests that execute it. Build and test pools are separate.

```
cargo install --path .
cargo quicksilver probe
cargo quicksilver list
cargo quicksilver estimate --package <crate> --sample 150
```

Survivor triage is opt-in. It combines each missed mutant with a nearby caught
control, reruns both constituents and the pair over the same tests, and reports
whether the evidence points to under-selection, a higher-order interaction,
masking, or bounded likely equivalence:

```
cargo quicksilver run --package <crate> --triage --triage-neighbors 1
```

An estimate can also execute a reproducible pool of disjoint second-order
mutant pairs. The reported first-order probability is the independence-model
inversion of the observed pair kill rate; it does not replace first-order
verdicts:

```
cargo quicksilver estimate --package <crate> --sample 150 --second-order-pairs 25
```

Every run persists the mutant-by-test results it actually observed in the
session's `index/coupling-v1.json`. Fail-fast rows remain explicitly partial;
only complete, comparable vectors are used to report mutation subsumption.
`--json` reports include `triage`, `second_order`, and `coupling` sections when
the corresponding evidence exists.

See `cargo quicksilver --help`.

## Research notes

The [`papers/`](papers/) directory contains research notes related to mutation
testing, causal program dependence, and higher-order mutants.

## License

This project is licensed under the GNU Lesser General Public License, version
3.0. See [LICENSE](LICENSE) for the complete license text.
