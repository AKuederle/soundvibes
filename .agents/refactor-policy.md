# Backwards-compatibility policy

- Absolutely no backwards compatibility is required. Soundvibes is experimental software used only on this PC.
- Internal refactors must use clean replacements and update every in-repository consumer in the same change. Do not add compatibility shims or transitional paths.
- CLI options, configuration fields, and Rust APIs may change freely. Document user-visible changes, but no downstream migration support is required.
