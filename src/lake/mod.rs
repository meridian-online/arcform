//! Publishing a Protocol's tables into a DuckLake catalogue held on S3-compatible
//! object storage (Cloudflare R2). Compiled under `http-fetch`, like `http_fetch`.

// Later tasks consume this module; until they land nothing calls it.
#![allow(dead_code)]

pub(crate) mod store;
