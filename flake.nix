{
  description = "pova - btop-style YouTube Music player with a built-in cava visualizer";

  inputs = {
    nixpkgs.url = "github:NixOS/nixpkgs/nixos-unstable";
  };

  outputs = { self, nixpkgs }:
    let
      systems = [ "x86_64-linux" "aarch64-linux" ];
      forAllSystems = f: nixpkgs.lib.genAttrs systems (system: f nixpkgs.legacyPackages.${system});
    in
    {
      packages = forAllSystems (pkgs:
        let
          cargoToml = builtins.fromTOML (builtins.readFile ./Cargo.toml);
          runtimeDeps = with pkgs; [
            mpv
            yt-dlp
            pipewire
            nodejs
          ];
        in
        {
          pova = pkgs.rustPlatform.buildRustPackage {
            pname = "pova";
            version = cargoToml.package.version;
            src = self;

            cargoLock = {
              lockFile = ./Cargo.lock;
            };

            nativeBuildInputs = [ pkgs.makeWrapper ];

            # Tests expect a TTY or local audio/config environment; unit tests run in CI.
            doCheck = false;

            postInstall = ''
              install -Dm755 scripts/import-cookie.sh $out/bin/pova-import-cookie
              install -Dm644 packaging/appimage/pova.desktop $out/share/applications/pova.desktop
              install -Dm644 packaging/appimage/pova.png $out/share/icons/hicolor/512x512/apps/pova.png
              wrapProgram $out/bin/pova \
                --prefix PATH : ${pkgs.lib.makeBinPath runtimeDeps}
            '';

            meta = with pkgs.lib; {
              description = cargoToml.package.description;
              homepage = cargoToml.package.repository;
              license = licenses.mit;
              platforms = platforms.linux;
              mainProgram = "pova";
            };
          };

          default = self.packages.${pkgs.system}.pova;
        });

      apps = forAllSystems (pkgs: {
        default = {
          type = "app";
          program = "${self.packages.${pkgs.system}.pova}/bin/pova";
        };
      });

      devShells = forAllSystems (pkgs: {
        default = pkgs.mkShell {
          packages = with pkgs; [
            rustc
            cargo
            rustfmt
            clippy
            mpv
            yt-dlp
            pipewire
            nodejs
          ];
        };
      });
    };
}
