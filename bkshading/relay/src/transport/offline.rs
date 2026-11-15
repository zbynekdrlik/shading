//! Why no camera answers (shading#14): the pure classification behind
//! [`bkshading_proto::wire::RelayState::offline_reason`].
//!
//! The session's camera lookup already sees each failure (no link, no answer, an HTTP answer that
//! is not the REST API); these functions turn it into the closed reason the panel explains.

use bkshading_proto::wire::OfflineReason;

use super::rest::RestError;

/// No address to try. `links_found` = a Blackmagic `cdc_ncm` link exists but has no DHCP lease
/// and no /30 address yet (the camera has not served DHCP: still booting, or its network function
/// is off); none = the camera is not on USB at all.
pub fn no_address_reason(links_found: bool) -> OfflineReason {
    if links_found {
        OfflineReason::CameraNotAnswering
    } else {
        OfflineReason::NoCameraLink
    }
}

/// A camera probe that failed with `e`.
///
/// - no HTTP answer at all, or a 5xx (a camera still booting answers 500/503) -> not answering;
/// - any other non-2xx (a 307 redirect, 404 everywhere, 401/403) or a 2xx that is not the
///   documented JSON (a web page) -> something answers HTTP but not the REST API: Web media
///   manager is off (PP cam7: 307, then 404).
pub fn probe_error_reason(e: &RestError) -> OfflineReason {
    match e {
        RestError::Transport(_) => OfflineReason::CameraNotAnswering,
        RestError::Status { status, .. } if *status >= 500 => OfflineReason::CameraNotAnswering,
        RestError::Status { .. } | RestError::Decode(_) => OfflineReason::RestOff,
    }
}

/// The reason to report when several addresses failed: an HTTP answer that is not REST says more
/// (the camera is there, its REST is off) than a silent address.
pub fn stronger(a: OfflineReason, b: OfflineReason) -> OfflineReason {
    let rank = |r: OfflineReason| match r {
        OfflineReason::RestOff => 3,
        OfflineReason::CameraNotAnswering => 2,
        OfflineReason::NoCameraLink => 1,
        OfflineReason::Unknown => 0,
    };
    if rank(b) > rank(a) {
        b
    } else {
        a
    }
}

/// The reason a camera lookup reports after it tried every candidate address: none tried =
/// [`no_address_reason`]; otherwise the [`stronger`] of every address's failure.
pub fn lookup_reason(
    links_found: bool,
    failures: impl IntoIterator<Item = OfflineReason>,
) -> OfflineReason {
    failures
        .into_iter()
        .fold(no_address_reason(links_found), stronger)
}

/// A read cycle on a camera found before in which NOT ONE endpoint answered (`missing` failures
/// out of `total` GETs) has lost the camera's REST API, e.g. Web media manager switched off while
/// the relay held the link: every endpoint answers 307/404. `None` while any endpoint answered.
pub fn lost_rest_reason(missing: &[(String, RestError)], total: usize) -> Option<OfflineReason> {
    if total == 0 || missing.len() < total {
        return None;
    }
    missing
        .iter()
        .map(|(_, e)| probe_error_reason(e))
        .reduce(stronger)
}

#[cfg(test)]
mod tests {
    use super::*;

    fn status(s: u16) -> RestError {
        RestError::Status {
            status: s,
            body: String::new(),
        }
    }

    #[test]
    fn each_probe_failure_maps_to_its_reason_14() {
        assert_eq!(
            probe_error_reason(&RestError::Transport("refused".into())),
            OfflineReason::CameraNotAnswering
        );
        assert_eq!(
            probe_error_reason(&status(503)),
            OfflineReason::CameraNotAnswering
        );
        assert_eq!(
            probe_error_reason(&status(500)),
            OfflineReason::CameraNotAnswering
        );
        assert_eq!(probe_error_reason(&status(307)), OfflineReason::RestOff);
        assert_eq!(probe_error_reason(&status(404)), OfflineReason::RestOff);
        assert_eq!(probe_error_reason(&status(403)), OfflineReason::RestOff);
        assert_eq!(
            probe_error_reason(&RestError::Decode("html".into())),
            OfflineReason::RestOff
        );
    }

    #[test]
    fn no_address_tells_a_missing_link_from_a_link_without_dhcp_14() {
        assert_eq!(no_address_reason(false), OfflineReason::NoCameraLink);
        assert_eq!(no_address_reason(true), OfflineReason::CameraNotAnswering);
    }

    #[test]
    fn rest_off_wins_over_a_silent_address_14() {
        use OfflineReason::*;
        assert_eq!(stronger(CameraNotAnswering, RestOff), RestOff);
        assert_eq!(stronger(RestOff, CameraNotAnswering), RestOff);
        assert_eq!(
            stronger(NoCameraLink, CameraNotAnswering),
            CameraNotAnswering
        );
        assert_eq!(
            stronger(CameraNotAnswering, NoCameraLink),
            CameraNotAnswering
        );
    }

    #[test]
    fn the_lookup_folds_every_address_14() {
        use OfflineReason::*;
        assert_eq!(lookup_reason(false, []), NoCameraLink);
        assert_eq!(lookup_reason(true, []), CameraNotAnswering);
        assert_eq!(lookup_reason(true, [CameraNotAnswering, RestOff]), RestOff);
        assert_eq!(lookup_reason(true, [RestOff, CameraNotAnswering]), RestOff);
        assert_eq!(
            lookup_reason(true, [CameraNotAnswering]),
            CameraNotAnswering
        );
    }

    #[test]
    fn a_read_with_no_endpoint_answering_has_lost_rest_14() {
        let m = |s: u16| ("/x".to_string(), status(s));
        assert_eq!(lost_rest_reason(&[], 2), None);
        assert_eq!(
            lost_rest_reason(&[m(404)], 2),
            None,
            "one endpoint answered"
        );
        assert_eq!(
            lost_rest_reason(&[m(307), m(404)], 2),
            Some(OfflineReason::RestOff)
        );
        assert_eq!(
            lost_rest_reason(&[m(503), m(503)], 2),
            Some(OfflineReason::CameraNotAnswering)
        );
        assert_eq!(
            lost_rest_reason(&[m(503), m(404)], 2),
            Some(OfflineReason::RestOff)
        );
        assert_eq!(lost_rest_reason(&[], 0), None);
    }
}
