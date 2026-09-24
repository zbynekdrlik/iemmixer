#!/usr/bin/env python3
"""Verify icon files match the tray.rs headphones design.

The tray icon (16x16) has:
- Two circular ear cups at (4,10) and (12,10) with radius 3
- Headband arc at top
- Vertical connectors at x=3 and x=13
- Color: #4A9EFF (74, 158, 255)
- All other pixels transparent (alpha=0)

This test verifies scaled icons preserve this design.
"""

from PIL import Image
import sys

BLUE = (0x4A, 0x9E, 0xFF)  # (74, 158, 255)
TOLERANCE = 15  # Color tolerance for scaled images


def get_expected_pixel(x, y, size):
    """Return expected pixel state for position (x,y) at given size.

    Returns: 'blue' if should be colored, 'transparent' if should be clear.
    Based on tray.rs design scaled to size.
    """
    # Map to 16x16 coordinate space
    scale = size / 16.0
    ox = x / scale
    oy = y / scale

    # Check if in left ear cup (center 4,10 radius 3)
    if (ox - 4) ** 2 + (oy - 10) ** 2 <= 3 ** 2 + 0.5:
        return 'blue'

    # Check if in right ear cup (center 12,10 radius 3)
    if (ox - 12) ** 2 + (oy - 10) ** 2 <= 3 ** 2 + 0.5:
        return 'blue'

    # Check headband (x from 3-13, y depends on x)
    if 3 <= ox <= 13:
        if ox < 5 or ox > 11:
            band_y = 4
        elif ox < 7 or ox > 9:
            band_y = 3
        else:
            band_y = 2
        if abs(oy - band_y) < 0.6:
            return 'blue'

    # Check connectors (x=3 or x=13, y from 5-7)
    if (abs(ox - 3) < 0.6 or abs(ox - 13) < 0.6) and 5 <= oy <= 7:
        return 'blue'

    return 'transparent'


def verify_icon(path: str) -> list[str]:
    """Verify an icon file matches tray design. Returns list of errors."""
    errors = []

    try:
        img = Image.open(path).convert("RGBA")
    except Exception as e:
        return [f"Failed to open {path}: {e}"]

    w, h = img.size

    # Test key points that MUST be correct

    # 1. Center of image should be transparent (gap between ear cups)
    cx, cy = w // 2, h // 2
    center = img.getpixel((cx, cy))
    if center[3] > 10:  # Should be nearly transparent
        errors.append(f"Center ({cx},{cy}) not transparent: alpha={center[3]}")

    # 2. Corners should be transparent
    corners = [(0, 0), (w-1, 0), (0, h-1), (w-1, h-1)]
    for x, y in corners:
        pixel = img.getpixel((x, y))
        if pixel[3] > 10:
            errors.append(f"Corner ({x},{y}) not transparent: alpha={pixel[3]}")

    # 3. Left ear cup center should be blue
    # In 16x16: center at (4, 10). Scale to current size.
    left_x = int(4 * w / 16)
    left_y = int(10 * h / 16)
    left = img.getpixel((left_x, left_y))
    if left[3] < 200:
        errors.append(f"Left ear ({left_x},{left_y}) not opaque: alpha={left[3]}")
    elif not (abs(left[0] - BLUE[0]) <= TOLERANCE and
              abs(left[1] - BLUE[1]) <= TOLERANCE and
              abs(left[2] - BLUE[2]) <= TOLERANCE):
        errors.append(f"Left ear ({left_x},{left_y}) wrong color: RGB=({left[0]},{left[1]},{left[2]}), expected ~{BLUE}")

    # 4. Right ear cup center should be blue
    right_x = int(12 * w / 16)
    right_y = int(10 * h / 16)
    right = img.getpixel((right_x, right_y))
    if right[3] < 200:
        errors.append(f"Right ear ({right_x},{right_y}) not opaque: alpha={right[3]}")
    elif not (abs(right[0] - BLUE[0]) <= TOLERANCE and
              abs(right[1] - BLUE[1]) <= TOLERANCE and
              abs(right[2] - BLUE[2]) <= TOLERANCE):
        errors.append(f"Right ear ({right_x},{right_y}) wrong color: RGB=({right[0]},{right[1]},{right[2]}), expected ~{BLUE}")

    # 5. Top center region (headband) should have blue pixels
    # Check a region scaled to image size since anti-aliased curves may not hit exact pixel
    band_found = False
    search_range = max(3, w // 16)  # Scale search area with image size
    for dy in range(-search_range, search_range + 1):
        for dx in range(-search_range, search_range + 1):
            bx = w // 2 + dx
            by = int(3 * h / 16) + dy  # y=3 is middle of headband range (2-4)
            if 0 <= bx < w and 0 <= by < h:
                pixel = img.getpixel((bx, by))
                if pixel[3] > 200:  # Found opaque blue pixel in headband region
                    band_found = True
                    break
        if band_found:
            break
    if not band_found:
        errors.append(f"Headband region (center top) has no opaque pixels")

    # 6. Headband must curve UPWARD (center y < edge y in screen coords)
    # tray.rs: center x=7-9 at y=2 (top), edges x=3-4 at y=4 (lower)
    # Find the y position of headband at center vs edge
    center_x = w // 2
    edge_x = int(3 * w / 16)
    s = max(2, w // 16)

    def find_band_y(x_pos):
        """Find the y position of the first opaque pixel in the headband region."""
        for y_scan in range(0, h // 2):
            pixel = img.getpixel((x_pos, y_scan))
            if pixel[3] > 128:
                return y_scan
        return None

    center_band_y = find_band_y(center_x)
    edge_band_y = find_band_y(edge_x)

    if center_band_y is not None and edge_band_y is not None:
        if center_band_y >= edge_band_y + s:
            errors.append(
                f"Headband is UPSIDE DOWN: center at y={center_band_y}, "
                f"edge at y={edge_band_y}. Center should be ABOVE (lower y) edge."
            )

    return errors


def main():
    icons = ["32x32.png", "128x128.png", "icon.png"]
    all_errors = []

    print("Verifying icons match tray.rs design...")
    print()

    for icon in icons:
        errors = verify_icon(icon)
        if errors:
            print(f"❌ {icon}:")
            for e in errors:
                print(f"   - {e}")
            all_errors.extend(errors)
        else:
            print(f"✓ {icon}: OK")

    print()

    if all_errors:
        print(f"FAILED: {len(all_errors)} error(s) found")
        print("Icons must match tray.rs headphones design.")
        sys.exit(1)
    else:
        print("PASSED: All icons match tray.rs design")
        sys.exit(0)


if __name__ == "__main__":
    main()
