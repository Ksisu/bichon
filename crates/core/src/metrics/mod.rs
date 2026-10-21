//
// Copyright (c) 2025-2026 rustmailer.com (https://rustmailer.com)
//
// This file is part of the Bichon Email Archiving Project
//
// This program is free software: you can redistribute it and/or modify
// it under the terms of the GNU Affero General Public License as published by
// the Free Software Foundation, either version 3 of the License, or
// (at your option) any later version.
//
// This program is distributed in the hope that it will be useful,
// but WITHOUT ANY WARRANTY; without even the implied warranty of
// MERCHANTABILITY or FITNESS FOR A PARTICULAR PURPOSE.  See the
// GNU Affero General Public License for more details.
//
// You should have received a copy of the GNU Affero General Public License
// along with this program.  If not, see <http://www.gnu.org/licenses/>.

//! Prometheus / OpenMetrics exposition (#162).
//!
//! The gauges are computed on every scrape from state that is either persisted
//! (accounts, download sessions) or cheap to read (index document counts), so
//! they survive restarts. The output is the OpenMetrics 1.0 text format,
//! terminated by `# EOF`.

use prometheus_client::encoding::{text::encode_eof, EncodeLabelValue, LabelValueEncoder};

use crate::{
    error::{code::ErrorCode, BichonResult},
    raise_error,
};

/// A label value enum with a bounded set of lowercase wire values. The
/// `EncodeLabelValue` derive would emit the Rust spelling (`Imap`), so the
/// encoding is written out here. Defined before the child modules so they can
/// use it.
macro_rules! label_values {
    ($vis:vis enum $name:ident { $($variant:ident => $value:literal),+ $(,)? }) => {
        #[derive(Clone, Copy, Debug, Hash, PartialEq, Eq)]
        $vis enum $name {
            $($variant),+
        }

        impl $name {
            pub const fn as_str(self) -> &'static str {
                match self {
                    $($name::$variant => $value),+
                }
            }
        }

        impl prometheus_client::encoding::EncodeLabelValue for $name {
            fn encode(
                &self,
                encoder: &mut prometheus_client::encoding::LabelValueEncoder<'_>,
            ) -> std::fmt::Result {
                std::fmt::Write::write_str(encoder, self.as_str())
            }
        }
    };
}

mod snapshot;

/// The `Content-Type` of [`render`]'s output.
pub const OPENMETRICS_CONTENT_TYPE: &str =
    "application/openmetrics-text; version=1.0.0; charset=utf-8";

/// A free-text label value, such as an account's email address or name.
///
/// prometheus-client writes `&str`/`String` label values verbatim, but the
/// OpenMetrics text format requires `\`, `"` and line feeds to be escaped. A
/// single unescaped quote in an account name would otherwise break the whole
/// scrape.
#[derive(Clone, Debug, Hash, PartialEq, Eq)]
pub(crate) struct LabelText(pub String);

impl EncodeLabelValue for LabelText {
    fn encode(&self, encoder: &mut LabelValueEncoder<'_>) -> std::fmt::Result {
        use std::fmt::Write;
        for c in self.0.chars() {
            match c {
                '\\' => encoder.write_str("\\\\")?,
                '"' => encoder.write_str("\\\"")?,
                '\n' => encoder.write_str("\\n")?,
                c => encoder.write_char(c)?,
            }
        }
        Ok(())
    }
}

/// Renders the complete scrape body ([`OPENMETRICS_CONTENT_TYPE`]).
///
/// A part that cannot be read (accounts, index, disk) is left out and
/// reported through `bichon_scrape_collector_success`, so a single failing
/// source never fails the whole scrape.
pub async fn render() -> BichonResult<String> {
    let snapshot = snapshot::collect().await;
    let mut out = String::with_capacity(16 * 1024);
    snapshot
        .encode(&mut out)
        .and_then(|()| encode_eof(&mut out))
        .map_err(|_| {
            raise_error!(
                "Failed to encode the OpenMetrics exposition".into(),
                ErrorCode::InternalError
            )
        })?;
    Ok(out)
}

#[cfg(test)]
mod tests {
    use prometheus_client::{
        encoding::{text::encode, EncodeLabelSet},
        metrics::{family::Family, gauge::Gauge},
        registry::Registry,
    };

    use super::*;

    #[derive(Clone, Debug, Hash, PartialEq, Eq, EncodeLabelSet)]
    struct NameLabel {
        name: LabelText,
    }

    #[test]
    fn label_text_escapes_backslash_quote_and_newline() {
        let family = Family::<NameLabel, Gauge>::default();
        family
            .get_or_create(&NameLabel {
                name: LabelText("Ops \"Team\" \\ N\nx".into()),
            })
            .set(1);
        let mut registry = Registry::default();
        registry.register("probe", "Escaping probe", family);
        let mut out = String::new();
        encode(&mut out, &registry).unwrap();
        assert!(
            out.contains(r#"probe{name="Ops \"Team\" \\ N\nx"} 1"#),
            "unexpected encoding:\n{out}"
        );
    }

    label_values!(enum Probe { First => "first", SecondValue => "second_value" });

    #[test]
    fn label_values_use_the_declared_wire_value() {
        assert_eq!(Probe::First.as_str(), "first");
        assert_eq!(Probe::SecondValue.as_str(), "second_value");
    }
}
