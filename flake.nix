{
  description = "Development environment for drv";

  inputs.nixpkgs.url = "github:NixOS/nixpkgs/nixos-unstable";

  outputs = { nixpkgs, ... }:
    let
      forAllSystems = nixpkgs.lib.genAttrs [
        "aarch64-linux"
        "x86_64-linux"
      ];
    in
    {
      devShells = forAllSystems (
        system:
        let
          pkgs = nixpkgs.legacyPackages.${system};
        in
        {
          default = pkgs.mkShell {
            packages = with pkgs; [
              cargo
              cargo-component
              clippy
              pkg-config
              rustc
              rustfmt
              wasmtime

              # Required by bindgen-based crates that consume Linux VFIO headers.
              linuxHeaders
              rustPlatform.bindgenHook
            ];

            BINDGEN_EXTRA_CLANG_ARGS = "-isystem ${pkgs.linuxHeaders}/include";
          };
        }
      );
    };
}
