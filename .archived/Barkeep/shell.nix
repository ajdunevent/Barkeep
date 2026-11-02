{ pkgs ? import <nixpkgs> {} }:

pkgs.mkShell {
  name = "barkeep-dev";

  nativeBuildInputs = [
    pkgs.pkg-config
    pkgs.cargo
    pkgs.rustc
    pkgs.rustfmt
    pkgs.clippy
    pkgs.cargo-deb
  ];

  buildInputs = [
    # GTK4 / libadwaita -- pulls in cairo, pango, gdk-pixbuf, graphene,
    # etc. transitively, same as before.
    pkgs.gtk4
    pkgs.libadwaita
    pkgs.librsvg # icon theme rendering (symbolic icons are SVG)

    # GStreamer, for camera capture (camera.rs)
    pkgs.gst_all_1.gstreamer
    pkgs.gst_all_1.gst-plugins-base
    pkgs.gst_all_1.gst-plugins-good
    pkgs.gst_all_1.gst-plugins-bad
    pkgs.gst_all_1.gst-plugins-rs # provides gtk4paintablesink
    pkgs.pipewire # provides the pipewiresrc element
  ];

  # Without this, GStreamer's plugin registry can't find the plugins
  # above at runtime even though they're on PKG_CONFIG_PATH for the
  # build -- this is what actually lets `gst::parse::launch` resolve
  # elements like `pipewiresrc` and `gtk4paintablesink`.
  shellHook = ''
    export GST_PLUGIN_SYSTEM_PATH_1_0="${pkgs.gst_all_1.gst-plugins-base}/lib/gstreamer-1.0:${pkgs.gst_all_1.gst-plugins-good}/lib/gstreamer-1.0:${pkgs.gst_all_1.gst-plugins-bad}/lib/gstreamer-1.0:${pkgs.gst_all_1.gst-plugins-rs}/lib/gstreamer-1.0"
  '';
}
