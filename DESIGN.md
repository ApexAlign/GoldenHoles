# Golden Shinobi - Design Document

## Game Overview
A fast-paced FPS with a "Digital Sandbox" aesthetic (Tron-inspired). Players navigate a neon grid, constructing and destroying the environment in real-time to dominate others in a Free-for-All (FFA) mode.

## Core Mechanics

### 1. Movement & Survival
*   **High-Velocity Movement**: Fluid and fast movement is central to the experience.
*   **Grappling Hook**: A primary traversal tool that supports both "pulling" and "swinging." Players should be able to orbit points or swing through space.
*   **No Fall Damage**: Gravity affects movement, but falling from any height does not cause damage. Survival depends on high-speed navigation and swinging.

### 2. Construction & Destruction (The Grid)
*   **Real-time Printing**: Players can "print" structures:
    *   `N` key: Create floors/ramps.
    *   `M` key: Create walls.
*   **Scaling**: Structures scale in size based on the player's kill count.
*   **Durability & De-rez**: 
    *   Structures have a health threshold before they "de-rez."
    *   **Longevity Bonus**: The longer a structure exists, the stronger (more durable) it becomes.
    *   **De-rez Effect**: When destroyed, structures dissolve into glowing pixels/data fragments rather than physical debris.

### 3. Game Flow & Aesthetics
*   **Visual Style**: "The Grid" - high-contrast neon colors, data streams, and geometric animations.
*   **Starting State**:
    *   Map begins almost entirely black (a void).
    *   Players spawn at opposite ends of the map.
    *   Each player is assigned a unique neon color.
*   **Objective**: Establish dominance by building structures and controlling territory starting from your respective end of the map.

## Technical Requirements
*   **Engine**: Bevy (Rust)
*   **Network**: FFA mode with low-latency synchronization for high-velocity movement.
