{
  config,
  lib,
  pkgs,
  ...
}:

let
  cfg = config.programs.nyi;
  settings = import ./settings.nix { inherit lib; };
in
{
  options.programs.nyi = settings.options // {
    enable = lib.mkEnableOption "nyi";

    package = lib.mkOption {
      type = lib.types.package;
      default = pkgs.callPackage ./package.nix { };
      defaultText = lib.literalExpression "pkgs.callPackage ./package.nix { }";
      description = "The nyi package to install.";
    };
  };

  config = lib.mkIf cfg.enable {
    environment.systemPackages = [ cfg.package ];
    environment.etc."xdg/nyi/config".text = settings.render cfg;
    environment.etc."xdg/nyi/logo.txt" = lib.mkIf (cfg.logo != null) {
      text = settings.renderLogo cfg;
    };
  };
}
