# Changelog

All notable changes to this project are documented here. The format follows
[Keep a Changelog](https://keepachangelog.com/en/1.1.0/) and this project
adheres to [Semantic Versioning](https://semver.org/spec/v2.0.0.html).

## [Unreleased]

### Added

- Cargo workspace scaffold: `pidgin-core` (library) and `pidgin-cli` (the
  `pidgin` binary), with an `pidgin run` placeholder command.
- CI (fmt, clippy, test), Dependabot, CODEOWNERS, and the fleet housekeeping,
  Straitjacket, conventional-commits, codespell, and vale workflows.
- PHP binding scaffold (M0): `bindings/php`, an ext-php-rs cdylib exposing
  `Pidgin::version()` through the `pidgin-core` façade, plus its build/test
  harness and a dedicated `php` CI job.
- `pidgin_core::version()`, surfacing the workspace version through the façade
  for bindings to report.
