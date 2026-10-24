//! The network, for the speech model's download.

use std::io::Read;

use scriber_core::ports::{Downloader, PortError, fail};
use ureq::tls::{RootCerts, TlsConfig, TlsProvider};

/// Blocking HTTP(S) through ureq: native TLS (Security.framework on macOS)
/// trusting the platform's roots, redirects followed, and an error status
/// treated as an error. Built with `HttpDownloader::default()`.
#[derive(Default)]
pub struct HttpDownloader(());

impl Downloader for HttpDownloader {
    fn open(&self, url: &str) -> Result<Box<dyn Read>, PortError> {
        let tls = TlsConfig::builder().provider(TlsProvider::NativeTls);
        let tls = tls.root_certs(RootCerts::PlatformVerifier).build();
        let agent = ureq::Agent::config_builder().tls_config(tls).build();
        let response = agent.new_agent().get(url).call().map_err(fail)?;
        Ok(Box::new(response.into_body().into_reader()))
    }
}
