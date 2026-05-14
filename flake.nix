{
  inputs.nixpkgs.url = "github:NixOS/nixpkgs/nixpkgs-unstable";

  outputs = { self, nixpkgs }:
    let
      system = "x86_64-linux";
      pkgs = nixpkgs.legacyPackages.${system};
    in {
      devShells.${system}.default = pkgs.mkShell {
        nativeBuildInputs = with pkgs; [ gcc lld ];
        buildInputs = with pkgs; [ stdenv.cc.cc.lib ];

        NICHY_EXTRA_LIBS = "${pkgs.stdenv.cc.cc.lib}/lib";

        shellHook = ''
          export LD_LIBRARY_PATH="$NICHY_EXTRA_LIBS:''${LD_LIBRARY_PATH:-}"
          export PATH="$PWD/bin:$PATH"
        '';
      };
    };
}
