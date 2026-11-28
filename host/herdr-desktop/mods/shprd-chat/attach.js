// Files for the composer: chosen with the system's file picker, dropped on the box, or
// pasted (a clipboard image becomes a PNG). The composer only keeps their paths: the
// service copies each one into the agent's own working directory when the message is
// sent, which is what lets the agent open it without a permission prompt
// (`crates/shprd-server/src/attachments.rs`).
//
// Shepherdr's chat has the same three entrances (`mods/chat/attach.js`); this is
// shprd's own code for them, not a copy.

import { appHome, exec } from "herdr";

const IMAGE = /\.(png|jpe?g|gif|webp|bmp|tiff?|heic)$/i;
const base = (path) => String(path).split("/").filter(Boolean).pop() ?? String(path);

/** Whether a path names a picture. */
export const isImage = (path) => IMAGE.test(String(path));

/** Where a pasted image is written before the message is sent. */
export const pasteDir = () => `${appHome()}/data/shprd-chat-attach`;

/** Paths (dropped, picked or pasted) as the entries the composer keeps. */
export function entries(paths) {
  return (paths ?? [])
    .map((path) => String(path ?? "").trim())
    .filter((path) => path.startsWith("/"))
    .map((path) => ({ path, name: base(path), image: isImage(path), temp: false }));
}

/** AppleScript: macOS's own file picker, several files, one POSIX path a line; nothing when cancelled. */
const MAC_PICK = [
  "on run argv",
  "  try",
  "    tell application (path to frontmost application as text) to set chosen to choose file with prompt (item 1 of argv) with multiple selections allowed",
  "  on error number -128",
  "    return \"\"",
  "  end try",
  "  set out to {}",
  "  repeat with f in chosen",
  "    set end of out to POSIX path of f",
  "  end repeat",
  "  set AppleScript's text item delimiters to linefeed",
  "  return out as text",
  "end run",
];

/**
 * The system's file picker. Resolves the chosen entries (`[]` when cancelled), or
 * throws a sentence when this machine has no picker.
 */
export async function pick(title = "Attach files") {
  const mac = await exec("uname", []).then((r) => r.stdout.trim() === "Darwin").catch(() => false);
  if (mac) {
    const r = await exec("osascript", [...MAC_PICK.flatMap((line) => ["-e", line]), title]);
    return entries(r.stdout.split("\n"));
  }
  const r = await exec("sh", [
    "-c",
    'command -v zenity >/dev/null || exit 127; zenity --file-selection --multiple --separator="\\n" --title="$1" 2>/dev/null; true',
    "x",
    title,
  ]);
  if (r.code === 127) throw new Error("This computer has no file picker (zenity). Drop the files on the box instead.");
  return entries(r.stdout.split("\n"));
}

/** JXA: macOS's clipboard: copied files as `file\t<path>` lines, else its image written as a PNG to argv[0] (`image`), else nothing. */
const MAC_CLIPBOARD = `ObjC.import("AppKit");
function run(argv) {
  const pb = $.NSPasteboard.generalPasteboard;
  const urls = pb.readObjectsForClassesOptions($([$.NSURL]), $({ NSPasteboardURLReadingFileURLsOnlyKey: true }));
  if (urls && !urls.isNil() && urls.count > 0) {
    const out = [];
    for (let i = 0; i < urls.count; i++) out.push("file\\t" + urls.objectAtIndex(i).path.js);
    return out.join("\\n");
  }
  let png = pb.dataForType($.NSPasteboardTypePNG);
  if (!png || png.isNil()) {
    const tiff = pb.dataForType($.NSPasteboardTypeTIFF);
    if (tiff && !tiff.isNil()) png = $.NSBitmapImageRep.imageRepWithData(tiff).representationUsingTypeProperties($.NSBitmapImageFileTypePNG, $({}));
  }
  if (png && !png.isNil() && png.writeToFileAtomically(argv[0], true)) return "image";
  return "";
}`;

/**
 * A paste that held no text: the clipboard's copied files, or its image written to
 * a PNG. `[]` when it holds neither; throws a sentence when it cannot be read.
 */
export async function fromClipboard(now = Date.now()) {
  const mac = await exec("uname", []).then((r) => r.stdout.trim() === "Darwin").catch(() => false);
  if (!mac) return [];
  const name = `pasted-${now}.png`;
  const dir = pasteDir();
  const r = await exec("sh", ["-c", 'mkdir -p "$1" && exec osascript -l JavaScript -e "$2" "$1/$3"', "x", dir, MAC_CLIPBOARD, name]);
  if (r.code !== 0) throw new Error("Couldn't read the clipboard.");
  const out = r.stdout.trim();
  if (out === "image") return [{ path: `${dir}/${name}`, name, image: true, temp: true }];
  return entries(out.split("\n").filter((line) => line.startsWith("file\t")).map((line) => line.slice(5)));
}

/** A pasted image that was removed or sent goes: only ones this mod wrote, never a person's own file. */
export function forget(file) {
  if (file?.temp && String(file.path).startsWith(`${pasteDir()}/`)) exec("rm", ["-f", file.path]).catch(() => {});
}
