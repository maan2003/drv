{ fetchFromGitHub, runCommand }:
let
  commit = "509ce3d952d550f93b544c8d94c99e798f09a9b4";
  linux = fetchFromGitHub {
    owner = "sc7280-mainline"; repo = "linux"; rev = commit;
    hash = "sha256-P//ujLX7OPljgXk/AAYP+MJCC4lwV7A7HC6WjKLeKO8=";
  };
in runCommand "ath11k-reference-${commit}" { passthru.referenceCommit = commit; } ''
  root="$out/reference/linux-${commit}"
  mkdir -p "$root/drivers/net/wireless/ath" "$root/drivers/soc/qcom" \
    "$root/include/trace/events" "$root/include/linux/soc/qcom"
  cp -R ${linux}/drivers/net/wireless/ath/ath11k "$root/drivers/net/wireless/ath/"
  cp ${linux}/drivers/soc/qcom/qmi_encdec.c "$root/drivers/soc/qcom/"
  cp ${linux}/include/linux/soc/qcom/qmi.h "$root/include/linux/soc/qcom/"
  cp ${linux}/include/trace/events/qrtr.h "$root/include/trace/events/"
  printf '%s\n' '${commit}' > "$root/COMMIT"
  chmod -R a-w "$out"
''
