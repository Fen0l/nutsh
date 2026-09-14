Closes #

## What to look at

<!-- The one thing a reviewer should read first. If a snapshot changed, say what changed in the
frame and why that is right. -->

## Checklist

- [ ] `make ci` is green
- [ ] a line under `Unreleased` in `CHANGELOG.md`, or this is not user-visible
- [ ] a new behaviour has a test that fails without it
- [ ] no hostname, address, site or account name anywhere in the diff
- [ ] touching `crates/prism`: the invariants in `CONTRIBUTING.md` were read first
