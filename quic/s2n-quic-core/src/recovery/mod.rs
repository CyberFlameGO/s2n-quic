// Copyright Amazon.com, Inc. or its affiliates. All Rights Reserved.
// SPDX-License-Identifier: Apache-2.0

pub use congestion_controller::CongestionController;
pub use cubic::CubicCongestionController;
pub use rtt_estimator::*;

pub mod congestion_controller;
pub mod cubic;
mod hybrid_slow_start;
mod pacing;
mod rtt_estimator;

//= https://www.rfc-editor.org/rfc/rfc9002#section-7.7
//# Senders SHOULD limit bursts to the initial congestion window; see
//# Section 7.2.

//= https://www.rfc-editor.org/rfc/rfc9002#section-7.2
//# Endpoints SHOULD use an initial congestion
//# window of ten times the maximum datagram size (max_datagram_size),
//# while limiting the window to the larger of 14,720 bytes or twice the
//# maximum datagram size.

//= https://www.rfc-editor.org/rfc/rfc9002#section-7.7
//= type=TODO
//= feature=Packet pacing
//= tracking-issue=1073
//# A sender with knowledge that the network path to the
//# receiver can absorb larger bursts MAY use a higher limit.
pub const MAX_BURST_PACKETS: u16 = 10;
