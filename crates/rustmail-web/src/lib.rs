//! # rustmail-web
//!
//! HTTP layer for RustMail Suite, built on axum.
//!
//! - [`app`]: application state, routing and cross-cutting middleware.
//! - [`auth`]: cookies, CSRF, rate limiting and the `/auth/*` JSON API.
//! - [`pages`]: server-rendered HTML pages and htmx partials.
//! - Quarantine pages (Phase 5) live in a private module and are routed from [`pages`].
//! - [`ui`]: askama templates, page context and error pages.
//! - [`i18n`]: Fluent localisation. [`assets`]: static files and the CDN/local choice.
#![deny(warnings)]
#![deny(missing_docs)]
#![forbid(unsafe_code)]

mod admin_common;
mod admin_health;
mod admin_pages;
mod admin_reports;
pub mod app;
pub mod assets;
pub mod auth;
pub mod config;
pub mod i18n;
pub mod mail;
mod mail_pages;
mod mail_ws;
pub mod pages;
mod quarantine_pages;
pub mod ui;

pub use app::{build_app, AppState};
