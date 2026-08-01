{
  description = "Development environment for drv";

  inputs.nixpkgs.url = "github:NixOS/nixpkgs/nixos-unstable";

  outputs =
    { nixpkgs, ... }:
    let
      forAllSystems = nixpkgs.lib.genAttrs [
        "aarch64-linux"
        "x86_64-linux"
      ];
    in
    {
      checks.x86_64-linux.vfio-edu =
        nixpkgs.legacyPackages.x86_64-linux.callPackage ./nix/vfio-edu-test.nix
          { };

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
              lld
              pkg-config
              rustc
              rustfmt
              wasmtime
              wasm-tools

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
