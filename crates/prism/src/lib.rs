//! Prism Central v4 HTTP client: auth, TLS, ETags, paging, rate limiting.

pub mod bucket;
pub mod client;
pub mod entity;
pub mod envelope;
pub mod error;
pub mod limits;
pub mod metrics;
pub mod profile;
pub mod valve;

pub use client::{
    ActionResult, Availability, Client, ClusterRef, ListOptions, NOT_SERVED, NamespaceStatus, Page,
    TaskRef,
};
pub use entity::Entity;
pub use error::{CertificateReason, PrismError};
pub use metrics::{Meter, Metrics, meter_text};
pub use profile::Profile;
