# Apache ECharts vendor artifact

- Package: `echarts`
- Version: `6.1.0`
- Source: npm package `echarts@6.1.0`
- npm integrity: `sha512-q0yaFPggC9FUdsWH4blavRWFmxdrIodbkoKNAjJudAI6CA9gNPxHtV2RcZNEepZVlk4yvBYkOkbk6HIVpIyHZA==`
- Bundled file: `dist/echarts.esm.min.mjs`
- Bundled file SHA-256: `28515f26aa57f87eb1e4ed4eb446927eaf329b5f5e02b4bfd4b0264fea40d1da`
- License: Apache-2.0; see `LICENSE.echarts` and `NOTICE.echarts`.

The ESM browser bundle is vendored because the telemetry dashboard is served as
self-contained Rust `include_str!` assets and intentionally has no runtime CDN
dependency or frontend package-manager requirement.
