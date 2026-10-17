//! Downloads through Windows' own HTTP stack, so system proxy settings and certificates apply.

use windows::Foundation::Uri;
use windows::Storage::Streams::DataReader;
use windows::Web::Http::HttpClient;
use windows::core::HSTRING;

/// Blocks until the whole body has arrived. Fails on a connection error or a non-2xx status.
pub fn get(url: &str) -> crate::Result<Vec<u8>> {
    let client = HttpClient::new()?;
    let response = client.GetAsync(&Uri::CreateUri(&HSTRING::from(url))?)?.join()?;
    response.EnsureSuccessStatusCode().map_err(|_| format!("{url} answered {}", response.StatusCode().map_or(0, |s| s.0)))?;
    let buffer = response.Content()?.ReadAsBufferAsync()?.join()?;
    let mut bytes = vec![0u8; buffer.Length()? as usize];
    DataReader::FromBuffer(&buffer)?.ReadBytes(&mut bytes)?;
    Ok(bytes)
}
