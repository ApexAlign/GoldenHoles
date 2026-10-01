//! Client layer. Owns everything the GPU draws (viewmodel, HUD, trails) and input capture.
//! Visual geometry stays here; the server never sees a mesh.

pub mod camera;
pub mod client_game;
pub mod font;
pub mod fx;
pub mod conn_watch;
pub mod demo_script;
pub mod hud_bridge;
pub mod hud_geom;
pub mod overlay;
pub mod pointer_lock;
pub mod hud;
pub mod input;
pub mod interp;
pub mod nav;
pub mod netclient;
pub mod quality;
pub mod reconcile;
pub mod screens;
pub mod settings;
pub mod touch_session;
pub mod touch_ui;
pub mod shaders;
pub mod trail;
pub mod udp_game;
pub mod udp_session;
pub mod emit;
pub mod viewmodel_draw;
pub mod remote;
pub mod viewmodel;
