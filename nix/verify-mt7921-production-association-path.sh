#!/usr/bin/env bash
set -euo pipefail
adapter=${1:?adapter source}
binary=${2:?installed ELF source}
fixture=${3:?ClientMlme fixture source}

grep -F 'pub fn prepare_production_wlan_frame(' "$adapter" >/dev/null
test "$(grep -Fc 'prepare_production_wlan_frame(' "$adapter")" -ge 2
grep -F 'prepare_production_wlan_frame(&buffer, self.support.association.as_ref())?' "$adapter" >/dev/null
grep -F 'association: Some(Default::default()),' "$binary" >/dev/null
grep -F 'raw_sae_h2e_association_request_fixture()' "$binary" >/dev/null
grep -F 'client_device::prepare_production_wlan_frame(' "$binary" >/dev/null
grep -F 'runtime_sha256 != "a0a6903ba753ebe8e8063804a448eacada51d5dd994c6f547a03fda49a90ce55"' "$binary" >/dev/null
grep -F 'runtime_sha256 == "8646ba36fe4d09133c784f4893e759e5d2e71de415a02642e5fa2a2adde89444"' "$binary" >/dev/null
grep -F 'pub fn raw_sae_h2e_association_request_fixture()' "$fixture" >/dev/null
grep -F 'linux_61840_oracle_profile()' "$fixture" >/dev/null
