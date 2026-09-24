#!/usr/bin/env python3
"""Generate icons that match the tray icon design with proper anti-aliasing.

The tray icon (16x16) design:
- Two circular ear cups at (4,10) and (12,10) with radius 3
- Headband arc at top
- Vertical connectors at x=3 and x=13
- Color: #4A9EFF (74, 158, 255)

For larger sizes: smooth anti-aliased rendering
For 16x16: exact pixel match to tray.rs
"""

from PIL import Image, ImageDraw
import struct
import io

# Exact color from tray.rs
BLUE = (0x4A, 0x9E, 0xFF)  # (74, 158, 255)
TRANSPARENT = (0, 0, 0, 0)


def draw_tray_icon_exact():
    """Draw the EXACT 16x16 tray icon design from tray.rs."""
    img = Image.new('RGBA', (16, 16), TRANSPARENT)

    def set_pixel(x, y):
        if 0 <= x < 16 and 0 <= y < 16:
            img.putpixel((x, y), BLUE + (255,))

    def draw_circle(cx, cy, r):
        for dy in range(r + 1):
            for dx in range(r + 1):
                if dx * dx + dy * dy <= r * r:
                    set_pixel(cx + dx, cy + dy)
                    set_pixel(cx + dx, cy - dy)
                    set_pixel(cx - dx, cy + dy)
                    set_pixel(cx - dx, cy - dy)

    # Left ear cup (circle at 4,10 with radius 3)
    draw_circle(4, 10, 3)

    # Right ear cup (circle at 12,10 with radius 3)
    draw_circle(12, 10, 3)

    # Headband arc at top
    for x in range(3, 14):
        if x < 5 or x > 11:
            y = 4
        elif x < 7 or x > 9:
            y = 3
        else:
            y = 2
        set_pixel(x, y)

    # Connect band to cups
    for y in [5, 6, 7]:
        set_pixel(3, y)
        set_pixel(13, y)

    return img


def draw_headphones_antialiased(size):
    """Draw smooth anti-aliased headphones at any size.

    Uses supersampling: render at 4x size then downsample for smooth edges.
    """
    # Supersample factor for anti-aliasing
    ss = 4
    large_size = size * ss

    img = Image.new('RGBA', (large_size, large_size), TRANSPARENT)
    draw = ImageDraw.Draw(img)

    # Scale factor from 16x16 reference
    s = large_size / 16.0

    # Ear cup parameters (from tray.rs: circles at 4,10 and 12,10 with radius 3)
    ear_radius = 3.2 * s  # Slightly larger for better visual at high res
    left_cx, left_cy = 4 * s, 10 * s
    right_cx, right_cy = 12 * s, 10 * s

    # Draw filled circles for ear cups with anti-aliasing
    draw.ellipse([
        left_cx - ear_radius, left_cy - ear_radius,
        left_cx + ear_radius, left_cy + ear_radius
    ], fill=BLUE + (255,))

    draw.ellipse([
        right_cx - ear_radius, right_cy - ear_radius,
        right_cx + ear_radius, right_cy + ear_radius
    ], fill=BLUE + (255,))

    # Headband - draw as smooth arc using bezier-like curve
    # From tray.rs: arc goes from x=3 to x=13, y varies 2-4
    band_width = 1.2 * s  # Thickness of headband

    # Create smooth arc points
    arc_points = []
    for i in range(21):  # 21 points for smooth curve
        t = i / 20.0  # 0 to 1
        x = 3 + t * 10  # x from 3 to 13
        # Parabolic curve: y=2 at center (top of arc), y=4 at edges (connects to cups)
        y = 2 + 2 * (2 * (t - 0.5)) ** 2
        arc_points.append((x * s, y * s))

    # Draw thick arc line
    if len(arc_points) >= 2:
        draw.line(arc_points, fill=BLUE + (255,), width=int(band_width), joint="curve")

    # Draw connectors (vertical lines from headband to ear cups)
    connector_width = 1.0 * s

    # Left connector (x=3, y from 5 to 7 in 16x16 space)
    draw.rectangle([
        3 * s - connector_width / 2, 4.5 * s,
        3 * s + connector_width / 2, 7.5 * s
    ], fill=BLUE + (255,))

    # Right connector (x=13, y from 5 to 7)
    draw.rectangle([
        13 * s - connector_width / 2, 4.5 * s,
        13 * s + connector_width / 2, 7.5 * s
    ], fill=BLUE + (255,))

    # Downsample with high-quality anti-aliasing
    img = img.resize((size, size), Image.LANCZOS)

    return img


def create_multi_ico(images, output_path):
    """Create a multi-resolution ICO file."""
    header = struct.pack('<HHH', 0, 1, len(images))

    entries = []
    png_data = []
    offset = 6 + 16 * len(images)

    for img in images:
        png_buffer = io.BytesIO()
        img.save(png_buffer, format='PNG')
        data = png_buffer.getvalue()
        png_data.append(data)

        w, h = img.size
        w_byte = 0 if w == 256 else w
        h_byte = 0 if h == 256 else h

        entry = struct.pack('<BBBBHHII',
            w_byte, h_byte, 0, 0, 1, 32, len(data), offset
        )
        entries.append(entry)
        offset += len(data)

    with open(output_path, 'wb') as f:
        f.write(header)
        for entry in entries:
            f.write(entry)
        for data in png_data:
            f.write(data)


def main():
    print("Generating anti-aliased icons matching tray.rs design...")

    # Generate base 16x16 icon (exact pixel match to tray icon)
    base = draw_tray_icon_exact()
    base.save('16x16-base.png', 'PNG')
    print("✓ Created 16x16-base.png (reference)")

    # Generate smooth anti-aliased icons at each size
    sizes = {
        '32x32.png': 32,
        '128x128.png': 128,
        'icon.png': 512,
    }

    for filename, size in sizes.items():
        img = draw_headphones_antialiased(size)
        img.save(filename, 'PNG')
        print(f"✓ Created {filename} (anti-aliased)")

    # Generate multi-resolution ICO
    ico_sizes = [16, 24, 32, 48, 64, 128, 256]
    ico_images = []
    for s in ico_sizes:
        if s == 16:
            ico_images.append(draw_tray_icon_exact())
        else:
            ico_images.append(draw_headphones_antialiased(s))
    create_multi_ico(ico_images, 'headphones.ico')
    print(f"✓ Created headphones.ico ({len(ico_sizes)} sizes, anti-aliased)")

    print("\nDone! Icons have smooth anti-aliased edges.")


if __name__ == "__main__":
    main()
