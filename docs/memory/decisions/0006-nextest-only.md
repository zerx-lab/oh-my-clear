---
status: accepted
date: 2026-09-28
tags: [testing]
---
# 0006 cargo-nextest is the only test runner; doctests disabled

## Context and Problem Statement
The user requires cargo-nextest instead of `cargo test`. nextest does not run doctests, and doctest code bypasses workspace lints (an `unwrap()` in a doc example would slip past the no-panic policy).

## Considered Options
* nextest + `cargo test --doc` exception
* nextest only; `doctest = false` in every `[lib]`; examples as nextest tests

## Decision Outcome
Chosen option: nextest only, no exceptions. `.config/nextest.toml` defines `default` (fail-fast) and `ci` (retries with `flaky-result = "fail"`, JUnit at `target/nextest/ci/junit.xml`) profiles. Rustdoc code blocks use ```` ```ignore ```` or ```` ```text ````. TTSR `no-cargo-test` interrupts any `cargo test` bash command.

### Consequences
* Good, because one runner, process-per-test isolation, retries/flake detection, and all example code is linted.
* Bad, because rustdoc examples are not compiled; zero-test crates need `--no-tests=warn` (L-0002).

## Evidence
- https://nexte.st/docs/running/ (doctests unsupported); docs/research/2026-09-28-rust-gates.md §3 (config keys verified against nextest 0.9.146)
