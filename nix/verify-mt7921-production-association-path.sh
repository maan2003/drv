#!/usr/bin/env bash
set -euo pipefail
adapter=${1:?adapter source}
binary=${2:?installed ELF source}
fixture=${3:?ClientMlme fixture source}

grep -F 'pub fn prepare_production_wlan_frame(' "$adapter" >/dev/null
grep -F 'fn prepare_production_wlan_frame_with_evidence(' "$adapter" >/dev/null
grep -F 'production_association_profile_from_query(' "$adapter" >/dev/null
grep -F 'let authoritative_ht = profile.ht_capabilities.ok_or(zx::Status::BAD_STATE)?;' "$adapter" >/dev/null
grep -F 'let authoritative_vht = profile.vht_capabilities.ok_or(zx::Status::BAD_STATE)?;' "$adapter" >/dev/null
grep -F 'effects.association_capability_transformation(evidence, frame);' "$adapter" >/dev/null
grep -F 'production_association_profile_from_query(' "$binary" >/dev/null
grep -F 'association_capability_transformation source={}' "$binary" >/dev/null
grep -F 'raw_sae_h2e_association_request_fixture()' "$binary" >/dev/null
grep -F 'client_device::prepare_production_wlan_frame(' "$binary" >/dev/null
grep -F 'two-stream device-query association fixture drifted' "$binary" >/dev/null
grep -F 'production association accepted missing authoritative HT/VHT inputs' "$binary" >/dev/null
grep -F 'authoritative HT/VHT transform retained base-dependent bytes' "$binary" >/dev/null
grep -F 'canonical_sha256 != "aa0306b8149896b679356f23657c7a77b49b82bfd4d485e3009831e4473437c4"' "$binary" >/dev/null
grep -F 'pub fn raw_sae_h2e_association_request_fixture()' "$fixture" >/dev/null
grep -F 'linux_61840_oracle_profile()' "$fixture" >/dev/null
