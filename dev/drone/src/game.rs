use rapier2d::prelude::*;
use tiny_skia::{Color, FillRule, Paint, PathBuilder, Pixmap, Stroke, Transform};

// Grid and rendering constants.
const GRID_SIZE: usize = 5;
const RESOLUTION: u32 = 720;
const TILE_PX: f32 = RESOLUTION as f32 / GRID_SIZE as f32; // 144

// Physics constants.
const DRONE_MAX_SPEED: f32 = 1.2; // tiles per second
const DRONE_APPROACH_DIST: f32 = 0.4; // start decelerating this far from target
const SNAP_DIST: f32 = 0.015; // snap to target when this close
const WIRE_LENGTH: f32 = 0.35; // target wire length in tile units
const WIRE_STIFFNESS: f32 = 80.0; // spring stiffness
const WIRE_DAMPING: f32 = 15.0; // spring damping
const TILT_FACTOR: f32 = 0.4; // max tilt angle in radians
const TILT_SMOOTHING: f32 = 8.0; // tilt lerp speed
const BATTERY_LOW: f32 = 10.0; // auto-dock threshold
const BATTERY_LIFE: f32 = 100.0; // seconds of battery life

/// Actions the viewer can send.
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum GameAction {
    Left,
    Right,
    Up,
    Down,
    Grab,
    Drop,
    Dock,
}

impl GameAction {
    pub fn from_str(s: &str) -> Option<Self> {
        match s {
            "left" => Some(Self::Left),
            "right" => Some(Self::Right),
            "up" => Some(Self::Up),
            "down" => Some(Self::Down),
            "grab" => Some(Self::Grab),
            "drop" => Some(Self::Drop),
            "dock" => Some(Self::Dock),
            _ => None,
        }
    }
}

/// Returns the list of action names for the status track.
pub fn action_names() -> Vec<String> {
    ["left", "right", "up", "down", "grab", "drop", "dock"]
        .iter()
        .map(|s| s.to_string())
        .collect()
}

/// The full game state: physics world + game logic.
pub struct GameState {
    // Rapier physics.
    pipeline: PhysicsPipeline,
    gravity: Vector,
    integration_params: IntegrationParameters,
    island_manager: IslandManager,
    broad_phase: BroadPhaseBvh,
    narrow_phase: NarrowPhase,
    bodies: RigidBodySet,
    colliders: ColliderSet,
    impulse_joints: ImpulseJointSet,
    multibody_joints: MultibodyJointSet,
    ccd_solver: CCDSolver,

    // Body handles.
    drone_handle: RigidBodyHandle,
    ball_handle: RigidBodyHandle,

    // Game logic.
    target: Option<(f32, f32)>,
    dock: (f32, f32),
    carrying: bool,
    wire_joint: Option<ImpulseJointHandle>,
    grabbing: bool, // spring pulling ball toward drone
    dropping: bool, // ball falling after release
    docking: bool,
    dock_path: Vec<(f32, f32)>,
    tilt: f32,         // current visual tilt angle
    battery: f32,      // 0..100
    low_battery: bool, // locked into auto-dock mode
}

impl GameState {
    pub fn new() -> Self {
        let mut bodies = RigidBodySet::new();
        let mut colliders = ColliderSet::new();

        // Drone: kinematic velocity-based, starts at dock (bottom-center).
        let dock = (2.0, 4.0);
        let drone_body = RigidBodyBuilder::kinematic_velocity_based()
            .translation(Vec2::new(dock.0, dock.1))
            .build();
        let drone_handle = bodies.insert(drone_body);

        // Drone collider (small circle, mainly for joint anchoring).
        let drone_collider = ColliderBuilder::ball(0.15)
            .sensor(true) // No physical collisions
            .build();
        colliders.insert_with_parent(drone_collider, drone_handle, &mut bodies);

        // Ground: static collider at the bottom of the visible grid.
        let ground_body = RigidBodyBuilder::fixed()
            .translation(Vec2::new(2.5, 4.35))
            .build();
        let ground_handle = bodies.insert(ground_body);
        let ground_collider = ColliderBuilder::cuboid(5.0, 0.05).build();
        colliders.insert_with_parent(ground_collider, ground_handle, &mut bodies);

        // Ball: dynamic, starts on ground at bottom row.
        let ball_body = RigidBodyBuilder::dynamic()
            .translation(Vec2::new(3.0, 4.1))
            .lock_rotations()
            .linear_damping(3.0) // friction to settle swinging faster
            .build();
        let ball_handle = bodies.insert(ball_body);

        let ball_collider = ColliderBuilder::ball(0.12).restitution(0.2).build();
        colliders.insert_with_parent(ball_collider, ball_handle, &mut bodies);

        let integration_params = IntegrationParameters {
            dt: 1.0 / 30.0,
            ..Default::default()
        };

        Self {
            pipeline: PhysicsPipeline::new(),
            gravity: Vec2::new(0.0, 15.0), // Y-down, moderate gravity
            integration_params,
            island_manager: IslandManager::new(),
            broad_phase: BroadPhaseBvh::new(),
            narrow_phase: NarrowPhase::new(),
            bodies,
            colliders,
            impulse_joints: ImpulseJointSet::new(),
            multibody_joints: MultibodyJointSet::new(),
            ccd_solver: CCDSolver::new(),
            drone_handle,
            ball_handle,
            target: None,
            dock,
            carrying: false,
            wire_joint: None,
            grabbing: false,
            dropping: false,
            docking: false,
            dock_path: Vec::new(),
            tilt: 0.0,
            battery: 100.0,
            low_battery: false,
        }
    }

    /// Current battery level (0..100).
    #[allow(dead_code)]
    pub fn battery(&self) -> f32 {
        self.battery
    }

    /// Current drone position in grid coordinates.
    fn drone_pos(&self) -> (f32, f32) {
        let t = self.bodies[self.drone_handle].translation();
        (t.x, t.y)
    }

    /// Current ball position.
    fn ball_pos(&self) -> (f32, f32) {
        let t = self.bodies[self.ball_handle].translation();
        (t.x, t.y)
    }

    /// Get the current target, defaulting to the drone's current nearest grid position.
    fn target_or_pos(&self) -> (f32, f32) {
        self.target.unwrap_or_else(|| {
            let (dx, dy) = self.drone_pos();
            (dx.round().clamp(0.0, 4.0), dy.round().clamp(0.0, 4.0))
        })
    }

    /// Apply a game action. Ignored if battery is critically low (auto-docking).
    pub fn apply_action(&mut self, action: GameAction) {
        if self.low_battery {
            return;
        }

        match action {
            GameAction::Left => {
                self.cancel_dock();
                let (tx, ty) = self.target_or_pos();
                self.target = Some(((tx - 1.0).max(0.0), ty));
            }
            GameAction::Right => {
                self.cancel_dock();
                let (tx, ty) = self.target_or_pos();
                self.target = Some(((tx + 1.0).min(4.0), ty));
            }
            GameAction::Up => {
                self.cancel_dock();
                let (tx, ty) = self.target_or_pos();
                self.target = Some((tx, (ty - 1.0).max(0.0)));
            }
            GameAction::Down => {
                self.cancel_dock();
                let (tx, ty) = self.target_or_pos();
                self.target = Some((tx, (ty + 1.0).min(4.0)));
            }
            GameAction::Grab => {
                if self.carrying || self.grabbing {
                    // Toggle: grab while carrying = start drop.
                    self.do_drop();
                    self.dropping = true;
                } else {
                    self.try_grab();
                }
            }
            GameAction::Drop => {
                if self.carrying || self.grabbing {
                    self.do_drop();
                    self.dropping = true;
                }
            }
            GameAction::Dock => {
                self.start_dock();
            }
        }
    }

    fn cancel_dock(&mut self) {
        self.docking = false;
        self.dock_path.clear();
    }

    fn try_grab(&mut self) {
        let (dx, dy) = self.drone_pos();
        let (bx, by) = self.ball_pos();

        // Drone must be within 1 tile of the ball.
        let dist = ((dx - bx).powi(2) + (dy - by).powi(2)).sqrt();
        if dist > 1.0 {
            return;
        }

        // Create spring joint — pulls ball toward drone with springy physics.
        let spring = SpringJointBuilder::new(WIRE_LENGTH, WIRE_STIFFNESS, WIRE_DAMPING)
            .local_anchor1(Vec2::new(0.0, 0.0))
            .local_anchor2(Vec2::new(0.0, 0.0));
        let handle = self
            .impulse_joints
            .insert(self.drone_handle, self.ball_handle, spring, true);
        self.wire_joint = Some(handle);
        self.grabbing = true;
    }

    fn do_drop(&mut self) {
        if let Some(handle) = self.wire_joint.take() {
            self.impulse_joints.remove(handle, true);
        }
        self.carrying = false;
        self.grabbing = false;
    }

    fn start_dock(&mut self) {
        let (dx, dy) = self.drone_pos();
        let gx = dx.round().clamp(0.0, 4.0);
        let gy = dy.round().clamp(0.0, 4.0);

        // Build path: first move to dock column, then to dock row.
        self.dock_path.clear();
        self.docking = true;

        // Move horizontally first.
        let steps_x = (self.dock.0 - gx) as i32;
        let mut cx = gx;
        for _ in 0..steps_x.unsigned_abs() {
            cx += steps_x.signum() as f32;
            self.dock_path.push((cx, gy));
        }

        // Then vertically.
        let steps_y = (self.dock.1 - gy) as i32;
        let mut cy = gy;
        for _ in 0..steps_y.unsigned_abs() {
            cy += steps_y.signum() as f32;
            self.dock_path.push((cx, cy));
        }

        // Start first waypoint, or target dock center directly if already on the dock tile.
        if let Some(&first) = self.dock_path.first() {
            self.target = Some(first);
        } else {
            self.target = Some((self.dock.0, self.dock.1));
        }
    }

    /// Advance the game by one physics step.
    pub fn tick(&mut self) {
        // Compute drone velocity toward target.
        if let Some((tx, ty)) = self.target {
            let (dx, dy) = self.drone_pos();
            let diff_x = tx - dx;
            let diff_y = ty - dy;
            let dist = (diff_x * diff_x + diff_y * diff_y).sqrt();

            if dist < SNAP_DIST {
                // Arrived at target.
                if let Some(drone) = self.bodies.get_mut(self.drone_handle) {
                    drone.set_translation(Vec2::new(tx, ty), true);
                    drone.set_linvel(Vec2::new(0.0, 0.0), true);
                }

                // Check dock path.
                if self.docking {
                    if let Some(pos) = self.dock_path.iter().position(|&p| p == (tx, ty)) {
                        self.dock_path.remove(pos);
                    }
                    if let Some(&next) = self.dock_path.first() {
                        self.target = Some(next);
                    } else {
                        self.target = None;
                        self.docking = false;
                    }
                } else {
                    self.target = None;
                }
            } else {
                // Move toward target with smooth speed profile.
                let speed = if dist < DRONE_APPROACH_DIST {
                    // Decelerate near target.
                    DRONE_MAX_SPEED * (dist / DRONE_APPROACH_DIST)
                } else {
                    DRONE_MAX_SPEED
                }
                .max(0.15); // minimum speed so we always reach target

                let vx = (diff_x / dist) * speed;
                let vy = (diff_y / dist) * speed;

                if let Some(drone) = self.bodies.get_mut(self.drone_handle) {
                    drone.set_linvel(Vec2::new(vx, vy), true);
                }
            }
        } else {
            // No target — stay still.
            if let Some(drone) = self.bodies.get_mut(self.drone_handle) {
                drone.set_linvel(Vec2::new(0.0, 0.0), true);
            }
        }

        // Check grab animation: ball reached drone (spring settled).
        if self.grabbing {
            let (dx, dy) = self.drone_pos();
            let (bx, by) = self.ball_pos();
            let dist = ((dx - bx).powi(2) + (dy - by).powi(2)).sqrt();
            if dist < WIRE_LENGTH + 0.1 {
                self.grabbing = false;
                self.carrying = true;
            }
        }

        // Check drop animation: ball landed (low velocity).
        if self.dropping {
            let ball_vel = self.bodies[self.ball_handle].linvel();
            let speed = (ball_vel.x * ball_vel.x + ball_vel.y * ball_vel.y).sqrt();
            if speed < 0.1 {
                self.dropping = false;
            }
        }

        // Battery: recharge on dock, drain elsewhere.
        let dt = self.integration_params.dt;
        let (dx, dy) = self.drone_pos();
        let on_dock = (dx - self.dock.0).abs() < 0.2 && (dy - self.dock.1).abs() < 0.2;

        if on_dock && self.target.is_none() {
            // Recharge at 5x drain rate.
            self.battery = (self.battery + (500.0 / BATTERY_LIFE) * dt).min(100.0);
            if self.low_battery && self.battery > 50.0 {
                self.low_battery = false; // Unlocked, can move again.
            }
        } else {
            self.battery = (self.battery - (100.0 / BATTERY_LIFE) * dt).max(0.0);
        }

        // Auto-dock when battery hits threshold.
        if !self.low_battery && self.battery <= BATTERY_LOW {
            self.low_battery = true;
            self.start_dock();
        }

        // Update tilt based on drone velocity.
        let drone_vel = self.bodies[self.drone_handle].linvel();
        let target_tilt = (drone_vel.x * TILT_FACTOR).clamp(-TILT_FACTOR, TILT_FACTOR);
        self.tilt += (target_tilt - self.tilt) * TILT_SMOOTHING * dt;

        // Step physics.
        self.pipeline.step(
            self.gravity,
            &self.integration_params,
            &mut self.island_manager,
            &mut self.broad_phase,
            &mut self.narrow_phase,
            &mut self.bodies,
            &mut self.colliders,
            &mut self.impulse_joints,
            &mut self.multibody_joints,
            &mut self.ccd_solver,
            &(),
            &(),
        );
    }

    /// Reset drone to dock position.
    #[allow(dead_code)]
    pub fn reset_to_dock(&mut self) {
        self.target = None;
        self.docking = false;
        self.dock_path.clear();

        if self.carrying || self.grabbing {
            self.do_drop();
        }
        self.dropping = false;

        if let Some(drone) = self.bodies.get_mut(self.drone_handle) {
            drone.set_translation(Vec2::new(self.dock.0, self.dock.1), true);
            drone.set_linvel(Vec2::new(0.0, 0.0), true);
        }
        self.tilt = 0.0;
    }

    /// Render the current game state to a pixmap.
    pub fn render(&self, pixmap: &mut Pixmap) {
        // Clear background.
        pixmap.fill(Color::from_rgba8(30, 30, 35, 255));

        self.draw_grid(pixmap);
        self.draw_dock(pixmap);
        self.draw_ball(pixmap);
        self.draw_wire(pixmap);
        self.draw_drone(pixmap);
        self.draw_battery(pixmap);
    }

    fn draw_grid(&self, pixmap: &mut Pixmap) {
        let mut paint = Paint::default();
        paint.set_color_rgba8(60, 60, 70, 255);
        paint.anti_alias = true;

        let stroke = Stroke {
            width: 1.5,
            ..Default::default()
        };

        // Vertical lines.
        for i in 0..=GRID_SIZE {
            let x = i as f32 * TILE_PX;
            let mut pb = PathBuilder::new();
            pb.move_to(x, 0.0);
            pb.line_to(x, RESOLUTION as f32);
            if let Some(path) = pb.finish() {
                pixmap.stroke_path(&path, &paint, &stroke, Transform::identity(), None);
            }
        }

        // Horizontal lines.
        for i in 0..=GRID_SIZE {
            let y = i as f32 * TILE_PX;
            let mut pb = PathBuilder::new();
            pb.move_to(0.0, y);
            pb.line_to(RESOLUTION as f32, y);
            if let Some(path) = pb.finish() {
                pixmap.stroke_path(&path, &paint, &stroke, Transform::identity(), None);
            }
        }
    }

    fn draw_dock(&self, pixmap: &mut Pixmap) {
        let mut paint = Paint::default();
        paint.set_color_rgba8(50, 120, 50, 180);
        paint.anti_alias = true;

        let (cx, cy) = self.to_screen(self.dock.0, self.dock.1);
        let w = TILE_PX * 0.6;
        let h = TILE_PX * 0.2;

        // Dock pad at bottom of tile.
        let rect = tiny_skia::Rect::from_xywh(cx - w / 2.0, cy + TILE_PX * 0.2, w, h);
        if let Some(rect) = rect {
            pixmap.fill_rect(rect, &paint, Transform::identity(), None);
        }

        // Dock outline.
        let mut outline_paint = Paint::default();
        outline_paint.set_color_rgba8(80, 180, 80, 255);
        outline_paint.anti_alias = true;

        let stroke = Stroke {
            width: 2.0,
            ..Default::default()
        };

        let mut pb = PathBuilder::new();
        pb.move_to(cx - w / 2.0, cy + TILE_PX * 0.2);
        pb.line_to(cx + w / 2.0, cy + TILE_PX * 0.2);
        pb.line_to(cx + w / 2.0, cy + TILE_PX * 0.2 + h);
        pb.line_to(cx - w / 2.0, cy + TILE_PX * 0.2 + h);
        pb.close();
        if let Some(path) = pb.finish() {
            pixmap.stroke_path(&path, &outline_paint, &stroke, Transform::identity(), None);
        }
    }

    fn draw_ball(&self, pixmap: &mut Pixmap) {
        let (bx, by) = self.ball_pos();
        let (sx, sy) = self.to_screen(bx, by);

        let mut paint = Paint::default();
        paint.set_color_rgba8(230, 180, 50, 255);
        paint.anti_alias = true;

        let r = TILE_PX * 0.12;

        // Draw filled circle.
        let mut pb = PathBuilder::new();
        pb.push_circle(sx, sy, r);
        if let Some(path) = pb.finish() {
            pixmap.fill_path(
                &path,
                &paint,
                FillRule::Winding,
                Transform::identity(),
                None,
            );
        }

        // Outline.
        let mut outline = Paint::default();
        outline.set_color_rgba8(255, 210, 80, 255);
        outline.anti_alias = true;
        let stroke = Stroke {
            width: 1.5,
            ..Default::default()
        };
        let mut pb = PathBuilder::new();
        pb.push_circle(sx, sy, r);
        if let Some(path) = pb.finish() {
            pixmap.stroke_path(&path, &outline, &stroke, Transform::identity(), None);
        }
    }

    fn draw_wire(&self, pixmap: &mut Pixmap) {
        if !self.carrying && !self.grabbing {
            return;
        }

        let (dx, dy) = self.drone_pos();
        let (bx, by) = self.ball_pos();
        let (sdx, sdy) = self.to_screen(dx, dy);
        let (sbx, sby) = self.to_screen(bx, by);

        let mut paint = Paint::default();
        paint.set_color_rgba8(150, 150, 160, 200);
        paint.anti_alias = true;

        let stroke = Stroke {
            width: 2.0,
            ..Default::default()
        };

        let mut pb = PathBuilder::new();
        pb.move_to(sdx, sdy);
        pb.line_to(sbx, sby);
        if let Some(path) = pb.finish() {
            pixmap.stroke_path(&path, &paint, &stroke, Transform::identity(), None);
        }
    }

    fn draw_drone(&self, pixmap: &mut Pixmap) {
        let (dx, dy) = self.drone_pos();
        let (sx, sy) = self.to_screen(dx, dy);

        // Drone body (rotated rectangle).
        let body_w = TILE_PX * 0.25;
        let body_h = TILE_PX * 0.12;

        let transform = Transform::from_translate(sx, sy)
            .pre_concat(Transform::from_rotate(self.tilt.to_degrees()));

        // Body.
        let mut paint = Paint::default();
        paint.set_color_rgba8(100, 160, 230, 255);
        paint.anti_alias = true;

        let rect = tiny_skia::Rect::from_xywh(-body_w / 2.0, -body_h / 2.0, body_w, body_h);
        if let Some(rect) = rect {
            pixmap.fill_rect(rect, &paint, transform, None);
        }

        // Propeller arms.
        let mut arm_paint = Paint::default();
        arm_paint.set_color_rgba8(80, 130, 200, 255);
        arm_paint.anti_alias = true;

        let arm_stroke = Stroke {
            width: 2.5,
            ..Default::default()
        };

        let arm_len = TILE_PX * 0.18;
        for &offset_x in &[-body_w / 2.0, body_w / 2.0] {
            // Arm line going up from body.
            let mut pb = PathBuilder::new();
            pb.move_to(offset_x, 0.0);
            pb.line_to(offset_x, -arm_len);
            if let Some(path) = pb.finish() {
                pixmap.stroke_path(&path, &arm_paint, &arm_stroke, transform, None);
            }

            // Propeller circle at tip.
            let mut pb = PathBuilder::new();
            pb.push_circle(offset_x, -arm_len, 4.0);
            if let Some(path) = pb.finish() {
                pixmap.fill_path(&path, &arm_paint, FillRule::Winding, transform, None);
            }
        }

        // Direction indicator (small triangle pointing up).
        let mut dir_paint = Paint::default();
        dir_paint.set_color_rgba8(200, 220, 255, 255);
        dir_paint.anti_alias = true;

        let mut pb = PathBuilder::new();
        pb.move_to(0.0, -body_h / 2.0 - 3.0);
        pb.line_to(-4.0, body_h / 2.0 - 1.0);
        pb.line_to(4.0, body_h / 2.0 - 1.0);
        pb.close();
        if let Some(path) = pb.finish() {
            pixmap.fill_path(&path, &dir_paint, FillRule::Winding, transform, None);
        }
    }

    fn draw_battery(&self, pixmap: &mut Pixmap) {
        let bar_w = 100.0_f32;
        let bar_h = 12.0_f32;
        let margin = 10.0_f32;
        let x = RESOLUTION as f32 - bar_w - margin;
        let y = margin;

        // Background.
        let mut bg = Paint::default();
        bg.set_color_rgba8(20, 20, 25, 200);
        if let Some(rect) = tiny_skia::Rect::from_xywh(x, y, bar_w, bar_h) {
            pixmap.fill_rect(rect, &bg, Transform::identity(), None);
        }

        // Fill — color based on level.
        let pct = (self.battery / 100.0).clamp(0.0, 1.0);
        let fill_w = bar_w * pct;

        let mut fill = Paint::default();
        if self.battery > 50.0 {
            fill.set_color_rgba8(74, 222, 128, 255); // green
        } else if self.battery > BATTERY_LOW {
            fill.set_color_rgba8(250, 204, 21, 255); // yellow
        } else {
            fill.set_color_rgba8(248, 113, 113, 255); // red
        }

        if fill_w > 0.5 {
            if let Some(rect) = tiny_skia::Rect::from_xywh(x, y, fill_w, bar_h) {
                pixmap.fill_rect(rect, &fill, Transform::identity(), None);
            }
        }

        // Border.
        let mut border = Paint::default();
        border.set_color_rgba8(100, 100, 110, 255);
        let stroke = Stroke {
            width: 1.0,
            ..Default::default()
        };
        let mut pb = PathBuilder::new();
        pb.move_to(x, y);
        pb.line_to(x + bar_w, y);
        pb.line_to(x + bar_w, y + bar_h);
        pb.line_to(x, y + bar_h);
        pb.close();
        if let Some(path) = pb.finish() {
            pixmap.stroke_path(&path, &border, &stroke, Transform::identity(), None);
        }

        // "LOW BATTERY" warning text overlay when critical.
        if self.low_battery {
            let mut warn = Paint::default();
            warn.set_color_rgba8(248, 113, 113, 200);
            // Draw a red bar across the top as a warning indicator.
            if let Some(rect) = tiny_skia::Rect::from_xywh(0.0, 0.0, RESOLUTION as f32, 3.0) {
                pixmap.fill_rect(rect, &warn, Transform::identity(), None);
            }
        }
    }

    /// Convert physics coordinates to screen pixel coordinates.
    fn to_screen(&self, x: f32, y: f32) -> (f32, f32) {
        (x * TILE_PX + TILE_PX / 2.0, y * TILE_PX + TILE_PX / 2.0)
    }
}
