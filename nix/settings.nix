{ lib }:

let
  inherit (lib) types mkOption;

  infoFields = [
    "os"
    "host"
    "kernel"
    "uptime"
    "packages"
    "shell"
    "display"
    "wm"
    "theme"
    "icons"
    "font"
    "cursor"
    "terminal"
    "cpu"
    "gpu"
    "memory"
    "swap"
    "disk"
    "ip"
    "battery"
    "locale"
    "colors"
  ];

  colorDoc = ''
    Accepts `red`, `green`, `yellow`, `blue`, `magenta`, `cyan`, `white`,
    or a raw ANSI number such as `"38;5;213"`.
  '';
in
rec {
  inherit infoFields;

  options = {
    info = mkOption {
      type = types.listOf (types.enum infoFields);
      default = infoFields;
      example = [
        "os"
        "kernel"
        "uptime"
        "cpu"
        "memory"
      ];
      description = ''
        info fields to display, in the order given. Only the fields listed
        here are shown.
      '';
    };

    labelColor = mkOption {
      type = types.str;
      default = "magenta";
      example = "38;5;213";
      description = "color used for info labels. " + colorDoc;
    };

    separator = mkOption {
      type = types.str;
      default = "-";
      description = ''
        String repeated to draw the underline below the `user@host` title.
        Set to `""` to leave the line blank.
      '';
    };

    box = mkOption {
      type = types.bool;
      default = false;
      description = "Draw a Unicode border box around the info block.";
    };

    shading = mkOption {
      type = types.nullOr types.str;
      default = null;
      example = ".,-~:;=!*#$@";
      description = ''
        Shading ramp, dimmest character first. Supports UTF-8. Setting this
        implies the `ascii` shading mode.
      '';
    };

    shadingMode = mkOption {
      type = types.nullOr (
        types.enum [
          "ascii"
          "blocks"
          "sextants"
        ]
      );
      default = null;
      description = ''
        `ascii` (the default) maps brightness onto the character ramp.
        `blocks` samples a 2x2 grid per cell and `sextants` a 2x3 one, for
        sharper edges. Sextants need a terminal that draws the Symbols for
        Legacy Computing block (kitty, Ghostty, foot, NewTerm).
      '';
    };

    logoOuter = mkOption {
      type = types.nullOr types.str;
      default = null;
      example = "magenta";
      description = ''
        color of the extruded sides, for logos that carry no ANSI colors of
        their own. ''
      + colorDoc;
    };

    logoInner = mkOption {
      type = types.nullOr types.str;
      default = null;
      example = "white";
      description = "color of the front and back faces. " + colorDoc;
    };

    light = mkOption {
      type = types.nullOr (
        types.enum [
          "top-left"
          "top-right"
          "top"
          "left"
          "right"
          "front"
          "bottom-left"
          "bottom-right"
        ]
      );
      default = null;
      description = "light direction for the 3D shading.";
    };

    spin = mkOption {
      type = types.nullOr (
        types.enum [
          "x"
          "y"
          "xy"
        ]
      );
      default = null;
      description = "axes the logo rotates around.";
    };

    speed = mkOption {
      type = types.nullOr types.float;
      default = null;
      description = "rotation speed multiplier. Higher is faster.";
    };

    size = mkOption {
      type = types.nullOr types.float;
      default = null;
      description = "logo scale, from 0.5 to 5.0.";
    };

    height = mkOption {
      type = types.nullOr types.int;
      default = null;
      description = ''
        render height in rows. left unset, nyi matches the number of info
        lines.
      '';
    };

    depth = mkOption {
      type = types.nullOr types.float;
      default = null;
      description = ''
        3D extrusion depth, 0.1 to 10.0. left unset nyi scales it
        from the logo's character variance so flat logos don't look
        paper-thin.
      '';
    };

    disks = mkOption {
      type = types.listOf types.str;
      default = [ ];
      example = [
        "/home"
        "/data"
      ];
      description = ''
        extra mount points to report beyond `/`, up to 8. Setting any turns
        the `disk` field on even if it isn't in {option}`info`.
      '';
    };

    logo = mkOption {
      type = types.nullOr types.lines;
      default = null;
      description = ''
        custom ASCII/Unicode logo, written to `nyi/logo.txt`. Denser
        characters (`@`, `M`, `#`) become taller in the relief.
      '';
    };

    logoDistro = mkOption {
      type = types.nullOr types.str;
      default = null;
      example = "nixos";
      description = ''
        distro name whose color scheme {option}`logo` should use, emitted as
        the `# distro:` hint line. Ignored unless {option}`logo` is set.
      '';
    };

    extraConfig = mkOption {
      type = types.lines;
      default = "";
      description = ''
        extra lines appended verbatim to the generated config file, for
        settings this module doesn't cover yet.
      '';
    };
  };

  /*
    the generated config file lines come first so their order is the
    display order; settings are order-independent to the parser.
  */
  render =
    cfg:
    let
      settings = lib.filterAttrs (_: v: v != null) {
        label_color = cfg.labelColor;
        separator = cfg.separator;
        box = if cfg.box then "1" else "0";
        shading = cfg.shading;
        shading_mode = cfg.shadingMode;
        logo_outer = cfg.logoOuter;
        logo_inner = cfg.logoInner;
        light = cfg.light;
        spin = cfg.spin;
        speed = if cfg.speed != null then toString cfg.speed else null;
        size = if cfg.size != null then toString cfg.size else null;
        height = if cfg.height != null then toString cfg.height else null;
        depth = if cfg.depth != null then toString cfg.depth else null;
      };
    in
    lib.concatStringsSep "\n" (
      cfg.info ++ map (d: "disk=${d}") cfg.disks ++ lib.mapAttrsToList (k: v: "${k}=${v}") settings
    )
    + "\n"
    + cfg.extraConfig;

  renderLogo =
    cfg: lib.optionalString (cfg.logoDistro != null) "# distro: ${cfg.logoDistro}\n" + cfg.logo;
}
