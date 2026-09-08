{
  lib,
  rustPlatform,
  runtimeShell,
}:

rustPlatform.buildRustPackage {
  pname = "drv-audio-pipewire-daemon";
  version = "0.1.0";
  src = ../..;
  cargoLock.lockFile = ../../Cargo.lock;
  cargoBuildFlags = [
    "-p"
    "drv-audio-pipewire-spike"
  ];
  cargoTestFlags = [
    "-p"
    "drv-audio-pipewire-spike"
  ];

  postInstall = ''
    substitute ${./private-daemon-launcher.sh} \
      "$out/bin/drv-audio-pipewire-private" \
      --replace-fail '@shell@' '${runtimeShell}' \
      --replace-fail '@daemon@' "$out/bin/drv-audio-pipewire-spike"
    chmod 0755 "$out/bin/drv-audio-pipewire-private"

    mkdir -p "$out/share/systemd/user"
    substitute ${./drv-audio-pipewire.service} \
      "$out/share/systemd/user/drv-audio-pipewire.service" \
      --replace-fail '@launcher@' "$out/bin/drv-audio-pipewire-private" \
      --replace-fail '@readme@' "$out/share/doc/drv-audio-pipewire/README.md"
    install -Dm644 ${./README.md} \
      "$out/share/doc/drv-audio-pipewire/README.md"
  '';

  meta = {
    description = "Opt-in private PipeWire endpoint backed by the Fuchsia ADR audio slice";
    license = with lib.licenses; [
      bsd2
      mit
      asl20
    ];
    mainProgram = "drv-audio-pipewire-private";
    platforms = lib.platforms.linux;
  };
}
