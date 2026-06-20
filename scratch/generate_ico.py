import base64
import os
import re

def main():
    ui_rs_path = r"c:\Users\qwe1579\Desktop\rustdesk-1.4.7\src\ui.rs"
    with open(ui_rs_path, "r", encoding="utf-8") as f:
        content = f.read()

    match = re.search(r'pub fn get_icon\(\) -> String \{\s*"data:image/png;base64,([^"]+)"', content)
    if not match:
        match = re.search(r'"data:image/png;base64,([^"]+)"', content)

    if not match:
        print("Could not find base64 icon in src/ui.rs")
        exit(1)

    base64_str = match.group(1).strip()
    png_bytes = base64.b64decode(base64_str)

    def build_single_ico(png_data):
        ico_header = b"\x00\x00\x01\x00\x01\x00"
        width = 48
        height = 48
        size = len(png_data)
        offset = 22
        
        directory = bytes([
            width,
            height,
            0, # palette count
            0, # reserved
            1, 0, # color planes
            32, 0, # bits per pixel
        ]) + size.to_bytes(4, "little") + offset.to_bytes(4, "little")
        
        return ico_header + directory + png_data

    # Try saving using PIL if available
    try:
        from PIL import Image
        import io
        
        img = Image.open(io.BytesIO(png_bytes))
        
        def save_ico(path):
            img.save(path, format="ICO", sizes=[(16, 16), (32, 32), (48, 48), (128, 128), (256, 256)])
            print(f"Saved multi-size ICO to {path} using Pillow")

    except ImportError:
        print("Pillow not found, writing single-size 48x48 ICO from raw PNG")
        def save_ico(path):
            ico_data = build_single_ico(png_bytes)
            with open(path, "wb") as f:
                f.write(ico_data)
            print(f"Saved single-size ICO to {path}")

    target_paths = [
        r"c:\Users\qwe1579\Desktop\rustdesk-1.4.7\res\icon.ico",
        r"c:\Users\qwe1579\Desktop\rustdesk-1.4.7\flutter\windows\runner\resources\app_icon.ico",
    ]

    for path in target_paths:
        os.makedirs(os.path.dirname(path), exist_ok=True)
        save_ico(path)

if __name__ == "__main__":
    main()
