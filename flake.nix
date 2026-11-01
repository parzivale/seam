{
  description = "seam development shell";

  inputs.nixpkgs.url = "github:NixOS/nixpkgs/nixos-unstable";

  outputs =
    { nixpkgs, ... }:
    let
      systems = [
        "x86_64-linux"
        "aarch64-linux"
        "aarch64-darwin"
      ];
      forAll = f: nixpkgs.lib.genAttrs systems (system: f nixpkgs.legacyPackages.${system});
    in
    {
      devShells = forAll (pkgs: {
        default = pkgs.mkShell {
          packages =
            with pkgs;
            [
              # Toolchains come from rust-toolchain.toml (and the shader
              # builder's `cargo +nightly-…`), which only rustup resolves.
              rustup
              # aws-lc-sys, under rustls.
              cmake
              clang
              perl
              go
              pkg-config
            ]
            ++ lib.optionals stdenv.hostPlatform.isLinux [
              vulkan-loader
              vulkan-validation-layers
              vulkan-tools
            ];

          # ash's `loaded` feature dlopens libvulkan; the loader then finds the
          # system driver under /run/opengl-driver.
          shellHook = pkgs.lib.optionalString pkgs.stdenv.hostPlatform.isLinux ''
            export LD_LIBRARY_PATH=${pkgs.lib.makeLibraryPath [ pkgs.vulkan-loader ]}''${LD_LIBRARY_PATH:+:$LD_LIBRARY_PATH}
            export VK_LAYER_PATH=${pkgs.vulkan-validation-layers}/share/vulkan/explicit_layer.d
          '';
        };
      });
    };
}
