{
  description = "SOCKS5 proxy over Reticulum Network Stack";

  inputs = {
    nixpkgs.url = "github:NixOS/nixpkgs/nixos-unstable";
  };

  outputs = { self, nixpkgs }:
    let
      supportedSystems = [ "x86_64-linux" "aarch64-linux" "aarch64-darwin" "x86_64-darwin" ];
      forAllSystems = nixpkgs.lib.genAttrs supportedSystems;

      pkgsFor = system: nixpkgs.legacyPackages.${system};

      mkPackage = pkgs: pkgs.rustPlatform.buildRustPackage {
        pname = "rns-proxy";
        version = "0.1.0";

        src = ./.;

        cargoLock.lockFile = ./Cargo.lock;

        cargoBuildFlags = [ "--bin" "rns-proxy" ];
        cargoTestFlags = [];

        meta = {
          description = "SOCKS5 proxy over Reticulum Network Stack";
          homepage = "https://github.com/mytecor/rns-proxy";
          license = pkgs.lib.licenses.mit;
          mainProgram = "rns-proxy";
        };
      };
    in
    {
      packages = forAllSystems (system: rec {
        rns-proxy = mkPackage (pkgsFor system);
        default = rns-proxy;
      });

      overlays.default = final: prev: {
        rns-proxy = mkPackage final;
      };

      devShells = forAllSystems (system:
        let pkgs = pkgsFor system; in {
          default = pkgs.mkShell {
            inputsFrom = [ (mkPackage pkgs) ];
            packages = with pkgs; [
              rustup
            ];
          };
        }
      );
    };
}
