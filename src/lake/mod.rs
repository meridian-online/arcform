//! Publishing a Protocol's tables into a DuckLake catalogue held on S3-compatible
//! object storage (Cloudflare R2). Compiled under `http-fetch`, like `http_fetch`.

#[expect(dead_code, reason = "used by the lake publish step in later tasks")]
pub(crate) mod store;

#[cfg_attr(
    not(test),
    expect(dead_code, reason = "used by the lake publish step in later tasks")
)]
pub(crate) mod meta;

pub(crate) mod commit;

#[cfg_attr(
    not(test),
    expect(dead_code, reason = "used by the lake publish step in later tasks")
)]
pub(crate) mod manifest;

#[cfg_attr(
    not(test),
    expect(dead_code, reason = "used by the lake publish step in later tasks")
)]
pub(crate) mod publish;

#[cfg(test)]
mod test_server;
