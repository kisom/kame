{
  description = "kame - Kyle's Marginalia Editor";

  inputs = {
    nixpkgs.url = "github:NixOS/nixpkgs/nixos-26.05";
    flake-utils.url = "github:numtide/flake-utils";
  };

  outputs = { self, nixpkgs, flake-utils }:
    flake-utils.lib.eachDefaultSystem (system:
      let
        pkgs = nixpkgs.legacyPackages.${system};

        # egui/eframe + wgpu: X11/Wayland windowing and Vulkan surface libs.
        # Fonts are embedded via include_bytes!, so no fontconfig/freetype needed.
        runtimeDeps = with pkgs; [
          libxkbcommon
          wayland
          vulkan-loader
          libx11
          libxcursor
          libxrandr
          libxi
        ];

        libPath = pkgs.lib.makeLibraryPath runtimeDeps;
      in
      {
        packages.default = pkgs.rustPlatform.buildRustPackage {
          pname = "kame";
          version = "0.1.0";
          src = ./.;
          cargoLock.lockFile = ./Cargo.lock;
          nativeBuildInputs = with pkgs; [ pkg-config makeWrapper ];
          buildInputs = runtimeDeps;
          # egui/wgpu load the X11/Wayland/Vulkan libs at runtime via dlopen,
          # so build-time linking does not put them on the RPATH; wrap the
          # binary with LD_LIBRARY_PATH so `nix run` / the installed package work.
          postFixup = ''
            wrapProgram $out/bin/kame --prefix LD_LIBRARY_PATH : ${libPath}
          '';
          meta.mainProgram = "kame";
        };

        apps.default = {
          type = "app";
          program = "${self.packages.${system}.default}/bin/kame";
        };

        devShells.default = pkgs.mkShell {
          buildInputs = runtimeDeps ++ (with pkgs; [
            cargo
            rustc
            clippy
            rustfmt
            rust-analyzer
            pkg-config
          ]);
          shellHook = ''
            export LD_LIBRARY_PATH="${libPath}:$LD_LIBRARY_PATH"
          '';
        };
      }
    );
}
