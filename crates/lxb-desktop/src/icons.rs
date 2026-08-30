//! Icon theme lookup and decoding.
//!
//! Implements the parts of the freedesktop icon theme specification that a
//! launcher actually needs: search the current theme, follow `Inherits` from
//! `index.theme`, fall back to hicolor and then `/usr/share/pixmaps`.

use std::collections::{HashMap, HashSet};
use std::path::{Path, PathBuf};

const ICON_EXTENSIONS: [&str; 3] = ["svg", "png", "xpm"];
const MAX_THEME_DEPTH: usize = 8;

/// Names for the glyphs the shell carries itself.
///
/// A colon, which no icon theme uses, so a built-in can never be shadowed by
/// an application that happens to install an icon of the same name.
pub const VOLUME: &str = "lxb:volume";
pub const VOLUME_MUTED: &str = "lxb:volume-muted";
pub const BRIGHTNESS: &str = "lxb:brightness";
/// The four tiles above the quick-settings bars: driving the pointer from the
/// right stick, the per-application volume mixer, whether anything is allowed
/// to interrupt, and what has been announced to the session while the user was
/// elsewhere.
///
/// Every glyph in the guide is lit by the same lamp above it, but these four
/// carry the most of that modelling: a gloss boundary that curves with the
/// object, shadow cast from one part onto the next, and controls sunk into
/// wells. They can afford it because they are the largest of the set — a tile
/// gives its glyph 42 px where a quick-settings bar gives 33 and the keyboard
/// hint 27, and a seam or a well drawn at 27 px is three grey pixels of
/// smudge. How much of the modelling each glyph keeps is a question of the
/// size it is drawn at and not of style; see brightness.svg for the reduction.
///
/// [`DO_NOT_DISTURB`] is the one mark in the shell that is knowingly a second
/// drawing of an object already in the set — [`SETTING_NIGHT_LIGHT`] is a moon
/// too. See do-not-disturb.svg for what keeps the two apart and why the rule
/// was bent for this one.
pub const POINTER_STICK: &str = "lxb:pointer-stick";
pub const VOLUME_MIXER: &str = "lxb:volume-mixer";
pub const DO_NOT_DISTURB: &str = "lxb:do-not-disturb";
pub const NOTIFICATIONS: &str = "lxb:notifications";
/// The two controller buttons the keyboard hint names. Drawn by position
/// rather than by letter, because A/B/X/Y are swapped between Xbox and
/// Nintendo pads and mean nothing at all on a PlayStation one, and Select is
/// branded Back, View, Share, Create or `−` depending on whose pad it is.
pub const PAD_SELECT: &str = "lxb:pad-select";
pub const PAD_WEST: &str = "lxb:pad-west";
/// The three more a panel names when it says what its buttons do: the two face
/// buttons that mean Accept and Back on every layout, and Start.
///
/// By position for the reason above, and Start by *where it sits on the pad*
/// for the same reason once more removed — it is a bar, a line, three lines or
/// a house depending on whose pad it is, and only its place is common to all.
pub const PAD_SOUTH: &str = "lxb:pad-south";
pub const PAD_EAST: &str = "lxb:pad-east";
pub const PAD_START: &str = "lxb:pad-start";
/// And the one that raises a menu — the top of the cluster, `Y` where a pad has
/// letters.
pub const PAD_NORTH: &str = "lxb:pad-north";
/// And the one in the middle of the pad, which is the way back to this shell
/// from anything running.
///
/// By where it sits once more, and this one has no other choice: it is a
/// sphere, a house, a logo, an oval or a letter depending on whose pad it is,
/// and not one of those is common to two of them.
pub const PAD_GUIDE: &str = "lxb:pad-guide";
/// The right mouse button, which is what raises a menu for anybody using a
/// pointer. Not a keyboard key: no key printed on a keyboard says "menu" to as
/// many people as the right button does.
pub const MOUSE_RIGHT: &str = "lxb:mouse-right";
/// And the same three said to somebody with their hands on a keyboard instead.
///
/// A cap rather than a letter, because the shell has no text to draw inside one
/// and three characters in a cap at the size a hint is drawn are three smudges.
/// The family is a rounded square where the pad's is a circle, which is what
/// says which kind of thing is being named — the two are never on screen
/// together, so the shape has to carry it alone.
pub const KEY_ESCAPE: &str = "lxb:key-escape";
pub const KEY_SPACE: &str = "lxb:key-space";
pub const KEY_ENTER: &str = "lxb:key-enter";
/// And the key that is this shell's guide button on a keyboard, drawn with the
/// diamond a Unix keyboard prints on it. The one mark for that key that is not
/// somebody's logo; see key-super.svg.
pub const KEY_SUPER: &str = "lxb:key-super";
/// The four arrow keys of the on-screen keyboard.
///
/// Drawn rather than lettered because Roboto — which the shell bundles so it
/// does not depend on what fonts a console has — carries no arrow glyphs, and
/// a keycap reading "Left" is not an arrow key.
pub const ARROW_LEFT: &str = "lxb:arrow-left";
pub const ARROW_DOWN: &str = "lxb:arrow-down";
pub const ARROW_UP: &str = "lxb:arrow-up";
pub const ARROW_RIGHT: &str = "lxb:arrow-right";
/// The on-screen keyboard's way out, drawn rather than lettered for the same
/// reason: it is the one key that has to be found without reading the board.
pub const KEYBOARD_HIDE: &str = "lxb:keyboard-hide";

/// One per column of the start screen's category row, in that row's order.
///
/// These are built in rather than looked up, which is a change from taking
/// `applications-graphics` and friends out of the icon theme. Three reasons,
/// and the first is the same one the quick-settings bars have: a machine with
/// no desktop on it has no theme to take them from, and an unlabelled row of
/// missing-icon squares is the one part of this shell that cannot degrade —
/// the row is the map of where everything lives.
///
/// The second is that a theme's application icons are *coloured*, and the
/// atlas multiplies the quad's colour into the texel. A coloured icon can only
/// come out muddier than the white it is tinted with, so the row ended up as a
/// line of dim blue and orange discs beside a shell drawn entirely in glass.
///
/// The third is that they were eleven icons from however many hands drew them.
/// These are eleven objects under one lamp, which is what a row is supposed to
/// look like.
pub const CATEGORY_SETTINGS: &str = "lxb:category-settings";
pub const CATEGORY_SYSTEM: &str = "lxb:category-system";
pub const CATEGORY_MULTIMEDIA: &str = "lxb:category-multimedia";
pub const CATEGORY_GRAPHICS: &str = "lxb:category-graphics";
pub const CATEGORY_INTERNET: &str = "lxb:category-internet";
pub const CATEGORY_OFFICE: &str = "lxb:category-office";
pub const CATEGORY_GAMES: &str = "lxb:category-games";
/// Where the machine gets more of itself from, and — at the other end of the
/// row — the other machine it can run inside itself. The two columns added
/// after this row was first drawn, and both drawn to the same standard for the
/// reason above: the row is the map, and a map with two theme icons in the
/// middle of it is eleven objects under one lamp and two under somebody else's.
pub const CATEGORY_SOFTWARE: &str = "lxb:category-software";
pub const CATEGORY_DEVELOPMENT: &str = "lxb:category-development";
pub const CATEGORY_EDUCATION: &str = "lxb:category-education";
pub const CATEGORY_UTILITIES: &str = "lxb:category-utilities";
pub const CATEGORY_WAYDROID: &str = "lxb:category-waydroid";
pub const CATEGORY_OTHER: &str = "lxb:category-other";

/// The subcategories a column carries, which are rows inside a column rather
/// than columns of the row above: Multimedia's two, and the one under
/// Graphics.
///
/// Drawn to the same standard all the same, and for a reason the theme cannot
/// help with either: a subcategory that fell back to the missing-icon square
/// would read as an application that will not start, which is the one thing a
/// way further in must never look like.
///
/// Each is also the mark of the *files* inside it, since what they hold is the
/// user's own music, films and photographs rather than applications — a track
/// with no cover art was drawn as the mark of its column on the console too.
pub const CATEGORY_MUSIC: &str = "lxb:category-music";
pub const CATEGORY_VIDEO: &str = "lxb:category-video";
pub const CATEGORY_IMAGES: &str = "lxb:category-images";
/// System's own subcategory: the disks, walked folder by folder.
///
/// Two folders rather than one, which is the same distinction [`CATEGORY_IMAGES`]
/// draws against `CATEGORY_GRAPHICS` — the row is about a quantity of files
/// rather than about a file — and it is deliberately the same distinction so
/// that the bar has one way of saying it rather than three.
pub const CATEGORY_FILES: &str = "lxb:category-files";

/// The marks the file explorer's own rows wear: a folder, a file, a volume,
/// and the user's own folder at the head of it all.
///
/// Four objects rather than a table of one per file type. What a `.pdf` is, is
/// written on the row in words the user can read; a cabinet of half-recognised
/// drawings under that would be the same information told worse, and told
/// wrongly the moment somebody keeps a format nothing has heard of. Where the
/// shell *does* already know a file — a song, a film, a photograph — it keeps
/// that shelf's own mark instead, so one file has one drawing wherever it is
/// being looked at from. See file-page.svg.
pub const FILE_FOLDER: &str = "lxb:file-folder";
pub const FILE_PAGE: &str = "lxb:file-page";
pub const FILE_DRIVE: &str = "lxb:file-drive";
pub const FILE_HOME: &str = "lxb:file-home";

/// The Steam column, and the mark every row that came from Steam wears.
///
/// Two drawings of one object rather than one drawing used twice, because the
/// two are asked different questions — see the files themselves. Built in like
/// the rest of the row: the column exists only while somebody is signed in,
/// and a machine with no icon theme must still be able to draw the column that
/// signing in produced.
///
/// Drawn here rather than taken from the Steam client's own icon for the
/// reason every other column glyph is: a coloured application icon comes out
/// muddy through an atlas that tints what it samples, and this shell's own
/// hand is what makes eleven columns look like one row.
pub const CATEGORY_STEAM: &str = "lxb:category-steam";
pub const STEAM: &str = "lxb:steam";

/// The two rows of the menu raised over the Steam entry itself, which are
/// about the *account* rather than about anything in its library: asking for
/// the library again, and leaving.
///
/// [`SIGN_OUT`] exists because the row wore [`UNINSTALL`] before it, and a
/// waste bin over Sign out says the wrong thing twice — nothing is removed, and
/// the account is still there to sign back into. [`REFRESH`] because the row
/// had no mark at all, which left the only two rows on that panel unable to
/// line up with one another.
///
/// Deliberately unlike [`SETTING_REFRESH`], the display's refresh rate: that is
/// one ring redrawing itself inside a monitor, and this is two arrows chasing
/// each other with no monitor anywhere near them.
pub const REFRESH: &str = "lxb:refresh";
pub const SIGN_OUT: &str = "lxb:sign-out";

/// The shell's own mark at the head of the System information
/// panel.
///
/// The one glyph in this set that stands for *this software* rather than for
/// something the user can do, which is why it has exactly one place to be. That
/// panel is where the machine says what it is, and the top line of what it is
/// running is this shell — so the mark belongs over the list in the way a
/// letterhead belongs over a letter, and not in a row of a column, where a row
/// carrying it would look like something to press.
///
/// Drawn untinted, unlike every other glyph here. A dialog's icon is the one
/// drawing in the shell the atlas is not asked to multiply an accent into — see
/// [`crate::ui`]'s `icon_quad`, where a slot that resolved is drawn white — and
/// the mark is authored in the same polished neutral material the rest of this
/// set is, so it comes out as itself over the glass rather than as a logo in
/// whichever colour the shell happens to be set to.
pub const LOGO: &str = "lxb:logo";

/// The shell's own rows in the Settings column, and the two marks a list of
/// values is made of.
///
/// Built in for the same reason the category row is: these are LineXinBar's own
/// settings, and a shell whose settings came up as blank squares on a machine
/// with no icon theme installed would be missing the one column it is entirely
/// responsible for. [`SWATCH`] is drawn white on purpose — the atlas
/// multiplies a quad's colour into the texel, so one drawing serves every
/// colour a list of them can hold.
pub const SETTING_APPEARANCE: &str = "lxb:setting-appearance";
pub const SETTING_ACCENT: &str = "lxb:setting-accent";
pub const SETTING_THEME: &str = "lxb:setting-theme";
pub const SETTING_WALLPAPER: &str = "lxb:setting-wallpaper";
pub const SETTING_ICONS: &str = "lxb:setting-icons";
pub const SETTING_DISPLAY: &str = "lxb:setting-display";
pub const SETTING_RESOLUTION: &str = "lxb:setting-resolution";
pub const SETTING_REFRESH: &str = "lxb:setting-refresh";
pub const SETTING_ORIENTATION: &str = "lxb:setting-orientation";
pub const SETTING_HDR: &str = "lxb:setting-hdr";

/// Settings > Display > OLED protection: resting a screen nobody is watching.
///
/// A display with a moon standing in its screen — [`SETTING_DISPLAY`]'s own
/// monitor with the shell's moon inside it, which is the object the page is
/// about doing the thing the page does. It goes on that folder and on the
/// switch inside it, the way the night light's moon is kept to the night light;
/// the screens listed under it wear [`SETTING_DISPLAY`], as they do everywhere
/// else in that tree.
pub const SETTING_SCREEN_REST: &str = "lxb:setting-screen-rest";

/// Settings > Display > Display order: which screen the compositor puts first.
///
/// The one glyph in this set with two screens in it, because it is the one page
/// under Display that is about the screens rather than about a screen — see the
/// file itself. It goes on that folder and nowhere else; the screens listed
/// inside it wear [`SETTING_DISPLAY`], as they do on every other page there.
pub const SETTING_ORDER: &str = "lxb:setting-order";

/// Settings > Display > Night light: the blue light filter, and the hours it
/// keeps.
///
/// [`SETTING_NIGHT_LIGHT`] is a crescent moon, drawn against [`BRIGHTNESS`]'s
/// sun on purpose — the same ball with the light taken off most of it. It goes
/// on the Night light folder and on the switch inside it, and nowhere else, the
/// way [`SETTING_HDR`] is kept to HDR.
///
/// [`SETTING_SCHEDULE`] is a clock face, worn by both hour rows: they are two
/// ends of one thing and the shell has one picture of a time. It is not the
/// moon, because the row above them already says which setting these hours
/// belong to and repeating it there would leave three identical marks down one
/// column.
pub const SETTING_NIGHT_LIGHT: &str = "lxb:setting-night-light";
pub const SETTING_SCHEDULE: &str = "lxb:setting-schedule";
/// The device everything on the machine records from, under Settings > Sounds.
///
/// The one row of that page with a drawing of its own rather than a borrowed
/// one: the output beside it wears [`VOLUME`], which is the speaker the volume
/// bar wears, because a second speaker drawn for this set could only be that
/// same speaker again or a worse one. There is no microphone anywhere else in
/// the shell to borrow, and an input device drawn as a speaker would be saying
/// the wrong thing rather than repeating a right one.
pub const SETTING_MICROPHONE: &str = "lxb:setting-microphone";
/// The Network page and its two halves.
///
/// The parent is deliberately none of the things inside it, and deliberately
/// not the globe [`CATEGORY_INTERNET`] already is: that column is the programs
/// a user reaches the world with, and this row is the machine's own connection.
/// See setting-network.svg for what it is instead and why.
pub const SETTING_NETWORK: &str = "lxb:setting-network";
pub const SETTING_WIFI: &str = "lxb:setting-wifi";
pub const SETTING_ETHERNET: &str = "lxb:setting-ethernet";
/// The one mark this shell wears outside a page: how strong the wireless link
/// is, beside the clock in the start screen's corner.
///
/// Three drawings of one object, each lighting one more arc of the same fan —
/// see [`crate::network::Signal`], which is what says which of them the corner
/// is showing. They are deliberately a second fan and not [`SETTING_WIFI`]: a
/// settings row draws that one at sixty-four pixels and needs one state, this
/// is drawn at thirty and needs three, and the sweep and the bead are what had
/// to give to make three of them tell each other apart at that size.
/// signal-strong.svg carries the whole of that reasoning.
pub const SIGNAL_WEAK: &str = "lxb:signal-weak";
pub const SIGNAL_FAIR: &str = "lxb:signal-fair";
pub const SIGNAL_STRONG: &str = "lxb:signal-strong";
/// The second mark this shell wears outside a page: what is left in the
/// battery, beside the clock and on the far side of it.
///
/// Six drawings of one object, on exactly the terms the fan above is three of
/// them — one shell with a different amount of water standing in it, so the
/// mark keeps its size and its place as the charge falls. Five levels rather
/// than the fan's three because the range is worth more steps: a battery is
/// read as *how much is left*, where a link is read as good or bad. See
/// [`crate::power::Level`], which is what says which of them the corner shows,
/// and battery-full.svg, which carries the geometry.
///
/// [`BATTERY_CHARGING`] is the sixth and stands outside the five: it is not a
/// level at all but the shell with a bolt in it, shown while the battery is
/// filling. What it costs is the level, and why that is the right trade at this
/// size is in battery-charging.svg.
pub const BATTERY_EMPTY: &str = "lxb:battery-empty";
pub const BATTERY_LOW: &str = "lxb:battery-low";
pub const BATTERY_HALF: &str = "lxb:battery-half";
pub const BATTERY_HIGH: &str = "lxb:battery-high";
pub const BATTERY_FULL: &str = "lxb:battery-full";
pub const BATTERY_CHARGING: &str = "lxb:battery-charging";
/// The Bluetooth column, and every row under it.
///
/// The rune, which is the one mark in this whole set that was not drawn for
/// this shell: it is what is printed on the side of every device the page is
/// about, and a shell that invented a better one would be asking the user to
/// learn a symbol in order to find the symbol they already know.
///
/// One mark for the column, the switch, the list and every device in it — the
/// way [`SETTING_WIFI`] serves the radio and each network under it. What tells
/// two pairs of headphones apart is their names.
pub const SETTING_BLUETOOTH: &str = "lxb:setting-bluetooth";
/// The two halves of what an interface is configured with, and they are a pair
/// that must not be confused: a tag *is* an address, a signpost is the thing
/// that knows where one leads. See setting-address.svg.
pub const SETTING_ADDRESS: &str = "lxb:setting-address";
pub const SETTING_NAME_SERVER: &str = "lxb:setting-name-server";
/// The mark on every row in the tree that is typed into rather than chosen —
/// the third way a value is set here, after the bead and the bar.
pub const SETTING_TYPED: &str = "lxb:setting-typed";
/// The three rows under one wireless network that do a thing rather than being
/// one of a set of answers — see [`crate::settings::action`].
///
/// The first two are one drawing in two states: two links of a chain, hooked
/// and snapped. They are of no particular medium, which is what a row under a
/// *wireless* network needs — a plug would be a picture of the wire this
/// connection has not got — and being the same object twice is what lets a
/// user read the second having seen the first.
///
/// The third — Forget — wears the waste bin [`UNINSTALL`] rather than a mark of
/// its own, because it is the same act on a different object: something the
/// machine was keeping is taken off it. One bin, wherever the shell removes
/// something, is worth more than two drawings that would each have to be
/// learnt.
pub const SETTING_CONNECT: &str = "lxb:setting-connect";
pub const SETTING_DISCONNECT: &str = "lxb:setting-disconnect";

/// The four turns under Settings > Display > Orientation, drawn as what they
/// are: one monitor, stood four ways, its stand saying which way up.
///
/// Values rather than subcategories, and the only values in the Settings tree
/// with drawings of their own — everything else there is a colour, a number or
/// a switch, and wears the bead [`SWATCH`] instead. These earn the exception
/// because what is being chosen *is* a shape: the row that matches the screen
/// in front of the user can be picked without reading it.
pub const SETTING_ROTATION_0: &str = "lxb:setting-rotation-0";
pub const SETTING_ROTATION_90: &str = "lxb:setting-rotation-90";
pub const SETTING_ROTATION_180: &str = "lxb:setting-rotation-180";
pub const SETTING_ROTATION_270: &str = "lxb:setting-rotation-270";
/// Settings > System, and the one row under it: how large applications draw
/// themselves.
///
/// [`SETTING_SYSTEM`] is a chip. It is neither the cog that stands for the whole
/// Settings column nor another monitor — every drawing of one in this set
/// belongs to a page under Display — because this is the page about the machine
/// rather than about its picture. See setting-system.svg.
///
/// [`SETTING_SCALE`] is a window and the larger window it is being drawn out to,
/// with one arrow along the diagonal. Deliberately unlike
/// [`SETTING_RESOLUTION`], which measures a *screen's* pixels across and down
/// inside a bezel: this has no bezel and one arrow, because what it changes is
/// one number that moves both dimensions at once, and it changes it for
/// applications rather than for the display.
pub const SETTING_SYSTEM: &str = "lxb:setting-system";
pub const SETTING_SCALE: &str = "lxb:setting-scale";

/// Settings > Users: the accounts this machine is for, and what makes another
/// one.
///
/// Three marks rather than one, and the division is the whole of what makes the
/// page readable. [`SETTING_USERS`] is *two* figures, because the row leads to a
/// set; [`SETTING_PERSON`] is one, because a row on that page is one account.
/// The page would be unreadable if the two were the same drawing: every row
/// under it either wears a photograph of somebody or wears this, and a fallback
/// that matched the heading above it would say only "a user" on a page where
/// every row is one.
///
/// [`SETTING_PERSON`] is also what Account type wears, on both forms. That is
/// the same drawing meaning the same thing — what kind of person this account is
/// — rather than a reuse: the values inside it are a set of alternatives and
/// wear the bead [`SWATCH`], like every other set in the tree.
///
/// [`SETTING_ADD_USER`] is that figure with a plus beside it, at the foot of the
/// column. Beside rather than cut into it: an opening in this set is a *control*
/// taken out of a bead — the pad's buttons, the padlock's keyhole — and a
/// person with a plus-shaped hole in them reads as something removed.
pub const SETTING_USERS: &str = "lxb:setting-users";
pub const SETTING_PERSON: &str = "lxb:setting-person";
pub const SETTING_ADD_USER: &str = "lxb:setting-add-user";
/// The rows a Users form is made of.
///
/// Six marks for seven rows, and the division is what makes a form legible at a
/// glance: three of its rows are typed into, and before this they wore
/// [`SETTING_TYPED`] — one drawing three times over, so the page read as a
/// stack of identical wells and the only thing telling them apart was the word
/// beside each.
///
/// [`SETTING_NAME`] is a name card, [`SETTING_USERNAME`] the at sign every
/// machine already means "the name you log in as" by, and [`SETTING_PASSWORD`]
/// a key — which both password rows share, because they are one question asked
/// twice.
///
/// The key is deliberately not [`AUTHENTICATE`]'s padlock. A padlock is the
/// thing that is *locked*, and it is what the panel polkit raises wears; a key
/// is what somebody types to get past one. Two marks for two moments.
///
/// The row that hands the form over wears [`CHOSEN`], which is the shell's one
/// tick. It wore the padlock once, and that read as the obstacle rather than as
/// agreeing to it; then it wore a bare tick of its own, which was a second
/// drawing for a job this set already had a drawing for. `CHOSEN` is not only
/// the badge on a value in force — it is what [`crate::apps::Entry::Pick`]
/// wears, the row pressed to commit a picker, which is the same act as the row
/// at the foot of a form. One tick, both places.
///
/// [`SETTING_AVATAR`] is a face in a picture frame rather than
/// [`CATEGORY_IMAGES`]'s mountain and sun. That one is the mark of *a picture*,
/// which is right for a wallpaper; an avatar is a picture of a *person*, and
/// which of the two it is is the whole question the row asks.
///
/// [`SETTING_ACCOUNT_TYPE`] is a shield. That row used to wear
/// [`SETTING_PERSON`], which is also what an account with no avatar falls back
/// to — one drawing standing for two different things two rows apart on one
/// page.
pub const SETTING_NAME: &str = "lxb:setting-name";
pub const SETTING_USERNAME: &str = "lxb:setting-username";
pub const SETTING_PASSWORD: &str = "lxb:setting-password";
pub const SETTING_AVATAR: &str = "lxb:setting-avatar";
pub const SETTING_ACCOUNT_TYPE: &str = "lxb:setting-account-type";

/// Settings > System > Picture-in-Picture: the small window a browser puts a
/// video into, floating over everything else.
///
/// One object across all six: a screen drawn as a frame, with the small window
/// standing inside it as a body. The window is what is made of water because
/// the window is what every one of these rows is about.
///
/// [`SETTING_PIP`] carries a play mark cut into that window, and the mark is
/// the whole difference between it and the four corners. This page asks whether
/// a video floats at all; those rows ask which corner it floats in, and a play
/// mark repeated four times down one column would say the first thing four more
/// times and the second not at all. It goes on the page and on the switch
/// inside it, the way the moon is kept to the night light.
///
/// [`SETTING_PIP_SIZE`] is the same screen with the same window drawn twice,
/// larger and smaller, both against the same edge — the edge, because moving it
/// as well would be answering the corner row's question here. Deliberately
/// unlike [`SETTING_SCALE`], the other size on this page: that one has an arrow
/// along a diagonal because it is about every application growing, and this is
/// two sizes of one small thing to choose between.
///
/// The four corners are the second set of values in the Settings tree with
/// drawings of their own, after the four orientations, and they earn it the
/// same way: what is being chosen is a place, and a place has a shape. The row
/// they hang on wears whichever of them is chosen — see
/// `settings::picture_in_picture_place`, and the battery's row, which is the
/// other mark in this shell that moves.
pub const SETTING_PIP: &str = "lxb:setting-pip";
pub const SETTING_PIP_SIZE: &str = "lxb:setting-pip-size";
pub const SETTING_PIP_TOP_LEFT: &str = "lxb:setting-pip-top-left";
pub const SETTING_PIP_TOP_RIGHT: &str = "lxb:setting-pip-top-right";
pub const SETTING_PIP_BOTTOM_LEFT: &str = "lxb:setting-pip-bottom-left";
pub const SETTING_PIP_BOTTOM_RIGHT: &str = "lxb:setting-pip-bottom-right";

/// A read-only explanation in Settings, visually distinct from the control it
/// sits beneath so an unavailable mode or capability is not mistaken for HDR.
pub const SETTING_INFO: &str = "lxb:setting-info";
pub const SWATCH: &str = "lxb:swatch";
pub const CHOSEN: &str = "lxb:chosen";

/// The row that makes one more of something.
///
/// The set has every verb for a thing that already exists and had none for
/// this one, so a row that adds wore whatever was nearest and said something
/// else by it. A plus needs no language: it is the same in every script this
/// shell is translated into.
pub const ADD: &str = "lxb:add";

/// The two context-menu rows that act on the application itself: removing it
/// from the machine, and starting it.
///
/// Built in like the rest, and drawn rather than taken from the icon theme for
/// the third reason the category row gives: `edit-delete` and
/// `media-playback-start` come from however many hands drew whatever theme is
/// installed, and the menu they sit in has one lamp over it.
///
/// [`UNINSTALL`] is the one mark in this set used from three places. Settings >
/// Network > Wi-Fi > Networks > *a saved network* > Forget wears it too, and
/// deliberately: what the bin means is *this is taken off the machine*, which
/// is as true of a network's saved profile as it is of a program. A second
/// drawing for the same act would be a second thing to learn.
///
/// The third is the Trash row under Files, which is the literal case — it is a
/// bin, and it is where everything the other two rows destroy would have gone
/// if it had been one of the user's own files. What that column's *own* head
/// row wears is [`TRASH_EMPTY`], and that one is a second drawing for a
/// reason: see it.
pub const UNINSTALL: &str = "lxb:uninstall";
pub const LAUNCH: &str = "lxb:launch";

/// The two rows of the menu over one of the user's own files that lead onward
/// rather than doing something: choosing which application opens it, and
/// choosing what order the column is listed in.
///
/// Drawn rather than themed for the same reason as the two above, and drawn
/// unlike each other on purpose: they sit four rows apart in one panel, and two
/// marks that both said "there is more this way" would leave the panel with two
/// rows the eye cannot tell apart.
pub const OPEN_WITH: &str = "lxb:open-with";
pub const SORT: &str = "lxb:sort";

/// The two rows of that menu that carry the file somewhere else, and the row
/// the journey ends on — see [`crate::transfer`].
///
/// Three marks rather than two because the third is not in the menu at all: it
/// stands at the head of every column of the folder the user is choosing, where
/// there is no list of commands round it to say what it is for. It has to say
/// so by itself, and a clipboard is what says it on every desktop there is.
///
/// [`COPY`] and [`MOVE`] sit two rows apart in one panel, so they are drawn as
/// unlike each other as the pair above them: two sheets against one sheet with
/// an arrow leaving it. What they have in common is the sheet, which is the
/// thing being carried, and that is the only thing they should have in common.
pub const COPY: &str = "lxb:copy";
pub const MOVE: &str = "lxb:move";
pub const PASTE: &str = "lxb:paste";

/// The row that changes what a file is called.
///
/// A pencil, and lying at an angle, which is the only mark in the set that
/// does: it sits two rows below the sheet with an arrow leaving it, and two
/// upright marks that far apart in one panel are two rows the eye has to read
/// rather than recognise.
pub const RENAME: &str = "lxb:rename";

/// The row at the head of a folder's column that makes a new folder in it.
///
/// A folder with a cross cut through it, which is what every file manager
/// draws for this — the mark has to say "a folder, and one that is not there
/// yet", and the only part of that the drawing can carry is the cross.
///
/// It is [`FILE_FOLDER`] with the cross pierced through the pocket rather than
/// a drawing of its own, because the row is about the rows underneath it: the
/// column it stands over is a column of folders wearing that mark, and a head
/// row drawn from a different family would be a row about something else.
pub const NEW_FOLDER: &str = "lxb:new-folder";

/// The row at the head of the Trash column that empties it.
///
/// [`UNINSTALL`]'s bin with the lid lifted off it, and a second bin rather
/// than that one for the one reason a second drawing is ever worth it: both
/// are on the screen at once. The Trash row in the column to the left wears
/// [`UNINSTALL`] and stays lit while this column is open, so two identical
/// marks one step apart would leave the head row saying what the row it hangs
/// from already said.
pub const TRASH_EMPTY: &str = "lxb:trash-empty";

/// The row that starts picking several rows at once, and the row at the head
/// of a column while somebody is.
///
/// [`CHOSEN`]'s tick in a square bead instead of a round one. It is the same
/// tick deliberately — what a tick means here is "this one", and this row is
/// the offer of that mark to rows the user picks out themselves — and it is a
/// different body because the two are on one panel together: Show hidden files
/// wears the round one when it is on, and at the size a menu draws a mark the
/// silhouette is the only thing that tells two ticks apart.
pub const SELECT_MULTIPLE: &str = "lxb:select-multiple";

/// The two rows at the head of a column of the user's own files: the field
/// that searches it, and the row that empties the field.
///
/// Built in for the same reason as everything above, and for one more that is
/// particular to them. These sit *in a column*, beside a row drawn with a
/// photograph out of the user's own collection and a row drawn with a note —
/// so they have to be of the same material as the rest of the shell, and an
/// icon theme's flat `edit-find` beside a glossy note would be the seam
/// showing.
pub const SEARCH: &str = "lxb:search";
pub const SEARCH_CLEAR: &str = "lxb:search-clear";

/// The guide menu's Screenshot row.
///
/// Built in like the two above, and for the third reason again: `camera-photo`
/// is a coloured icon from whichever theme is installed, and the row it sits in
/// has one lamp over it and two arrows drawn under that lamp already.
pub const SCREENSHOT: &str = "lxb:screenshot";

/// The panel that asks the user to prove they may do something — see
/// [`crate::polkit`].
///
/// Built in, and here rather than taken from the icon theme for a reason none
/// of the others have. polkit hands the agent an icon *name* with every
/// question, chosen by whoever wrote the policy file, and it names an icon out
/// of whatever theme happens to be installed. Two things are wrong with using
/// it: the shell stands an application icon in for any name it cannot find, so
/// a panel asking for a password could come up wearing the generic executable
/// mark, and a themed icon on the one panel in the shell that takes a password
/// is a picture chosen by the program doing the asking. One padlock, always the
/// same one, is the honest answer.
pub const AUTHENTICATE: &str = "lxb:authenticate";

/// The three transport buttons on the guide's media card, and the two faces of
/// the middle one.
///
/// Four marks for three buttons: the button in the middle is the one control in
/// the shell whose glyph says what pressing it will *do* rather than what it
/// is, so it carries whichever of play and pause the player is not already.
pub const MEDIA_PREVIOUS: &str = "lxb:media-previous";
pub const MEDIA_PLAY: &str = "lxb:media-play";
pub const MEDIA_PAUSE: &str = "lxb:media-pause";
pub const MEDIA_NEXT: &str = "lxb:media-next";

/// The bar that sets how loud the thing being played is, as against how loud
/// the session is. A single note; the beamed pair means a shelf of music — see
/// [`CATEGORY_MUSIC`].
pub const MEDIA_VOLUME: &str = "lxb:media-volume";

/// The power button at the foot of the guide's sidebar.
///
/// The shell used to assemble this out of two solid quads — a ring with a
/// notch cut in it and a rod above — which was the last flat mark left in that
/// column. `ui::power_glyph` still exists and still draws it, as the fallback
/// for a glyph that somehow failed to rasterise: of everything in the sidebar
/// this is the one button that must never come up empty, because it has no
/// label to fall back on.
pub const SHUTDOWN: &str = "lxb:shutdown";

/// What every cell cut from the shell's own type is filed under.
///
/// A prefix rather than a list, because what is behind one of these names is
/// decided by how it was made and not by which name it is: a letter cell is a
/// *measurement* of a character's shape — see [`distance_field`], and
/// `gpu::letter_fields`, which is the only thing that puts one in the atlas. So
/// [`shaped`] can answer for the whole family at once, and a name filed here
/// that nothing cut is simply a name the atlas has not got.
pub const LETTER_PREFIX: &str = "lxb:letter-";

/// The characters a long list can be indexed by, and the cell each is cut into.
///
/// The Latin alphabet and one heap for everything else, which is a bounded set
/// on purpose. These are cut from the bundled face at startup, one exact
/// distance transform each, and drawn as marks — so the set is a fixed cost the
/// shell pays once and not a way of turning any string into a picture. The
/// corner's clock is the same argument with thirteen characters in it; see
/// `gpu::LETTER_SET`, which is deliberately still its own list because a clock
/// and an index share no character but their material.
///
/// Uppercase, because a heading is a capital. `#` is where a name that starts
/// with anything else goes — a digit, a bracket, an alphabet this set has no
/// letter of — and it is one row rather than an alphabet per script for the
/// same reason the set is bounded at all.
pub const INDEX_LETTERS: [(char, &str); 27] = [
    ('#', "lxb:letter-hash"),
    ('A', "lxb:letter-a"),
    ('B', "lxb:letter-b"),
    ('C', "lxb:letter-c"),
    ('D', "lxb:letter-d"),
    ('E', "lxb:letter-e"),
    ('F', "lxb:letter-f"),
    ('G', "lxb:letter-g"),
    ('H', "lxb:letter-h"),
    ('I', "lxb:letter-i"),
    ('J', "lxb:letter-j"),
    ('K', "lxb:letter-k"),
    ('L', "lxb:letter-l"),
    ('M', "lxb:letter-m"),
    ('N', "lxb:letter-n"),
    ('O', "lxb:letter-o"),
    ('P', "lxb:letter-p"),
    ('Q', "lxb:letter-q"),
    ('R', "lxb:letter-r"),
    ('S', "lxb:letter-s"),
    ('T', "lxb:letter-t"),
    ('U', "lxb:letter-u"),
    ('V', "lxb:letter-v"),
    ('W', "lxb:letter-w"),
    ('X', "lxb:letter-x"),
    ('Y', "lxb:letter-y"),
    ('Z', "lxb:letter-z"),
];

/// The mark on the row an index hangs under: the alphabet named by its two
/// ends, cut from the same face as the headings inside it.
///
/// A row's mark says what is behind the row, and what is behind this one is
/// [`INDEX_LETTERS`] — so the honest mark is the letters themselves, in the
/// material every heading in that column is written in. The Steam mark stood
/// here first and said the wrong thing twice over: every row in that column
/// came from Steam, so it distinguished nothing, and the column it opens is
/// the one place in the shell where the marks *are* the reading order.
///
/// Three characters in one cell rather than three cells, because this is one
/// mark and a row has one. It is therefore cut in a box wide enough to hold the
/// run, which leaves it shorter than a single heading beside it — see
/// `gpu::INDEX_MARK_BOX`.
pub const INDEX_MARK: &str = "lxb:letter-az";

/// The cell one of the index's headings is drawn from, if it is one of them.
pub fn letter_mark(letter: char) -> Option<&'static str> {
    INDEX_LETTERS
        .iter()
        .find(|(heading, _)| *heading == letter)
        .map(|(_, name)| *name)
}

/// Every built-in, as `(name, drawing)`, for the atlas to load at startup.
///
/// Compiled into the binary from files in the tree, the way the font and the
/// shaders are. Two things follow from that, and both are the point: the shell
/// has these whatever is installed on the machine — which is what the
/// quick-settings bars are *for* — and they are still drawings, editable in
/// anything that opens an SVG rather than in a string literal.
pub const BUILTIN: [(&str, &str); 128] = [
    (VOLUME, include_str!("glyphs/volume.svg")),
    (VOLUME_MUTED, include_str!("glyphs/volume-muted.svg")),
    (BRIGHTNESS, include_str!("glyphs/brightness.svg")),
    (MEDIA_VOLUME, include_str!("glyphs/media-volume.svg")),
    (MEDIA_PREVIOUS, include_str!("glyphs/media-previous.svg")),
    (MEDIA_PLAY, include_str!("glyphs/media-play.svg")),
    (MEDIA_PAUSE, include_str!("glyphs/media-pause.svg")),
    (MEDIA_NEXT, include_str!("glyphs/media-next.svg")),
    (POINTER_STICK, include_str!("glyphs/pointer-stick.svg")),
    (VOLUME_MIXER, include_str!("glyphs/volume-mixer.svg")),
    (DO_NOT_DISTURB, include_str!("glyphs/do-not-disturb.svg")),
    (NOTIFICATIONS, include_str!("glyphs/notifications.svg")),
    (PAD_SELECT, include_str!("glyphs/pad-select.svg")),
    (PAD_WEST, include_str!("glyphs/pad-west.svg")),
    (PAD_SOUTH, include_str!("glyphs/pad-south.svg")),
    (PAD_EAST, include_str!("glyphs/pad-east.svg")),
    (PAD_START, include_str!("glyphs/pad-start.svg")),
    (PAD_NORTH, include_str!("glyphs/pad-north.svg")),
    (PAD_GUIDE, include_str!("glyphs/pad-guide.svg")),
    (MOUSE_RIGHT, include_str!("glyphs/mouse-right.svg")),
    (KEY_ESCAPE, include_str!("glyphs/key-escape.svg")),
    (KEY_SPACE, include_str!("glyphs/key-space.svg")),
    (KEY_ENTER, include_str!("glyphs/key-enter.svg")),
    (KEY_SUPER, include_str!("glyphs/key-super.svg")),
    (ARROW_LEFT, include_str!("glyphs/arrow-left.svg")),
    (ARROW_DOWN, include_str!("glyphs/arrow-down.svg")),
    (ARROW_UP, include_str!("glyphs/arrow-up.svg")),
    (ARROW_RIGHT, include_str!("glyphs/arrow-right.svg")),
    (KEYBOARD_HIDE, include_str!("glyphs/keyboard-hide.svg")),
    (SHUTDOWN, include_str!("glyphs/shutdown.svg")),
    // The category row, in the order it is laid out in.
    (
        CATEGORY_SETTINGS,
        include_str!("glyphs/category-settings.svg"),
    ),
    (CATEGORY_SYSTEM, include_str!("glyphs/category-system.svg")),
    (
        CATEGORY_MULTIMEDIA,
        include_str!("glyphs/category-multimedia.svg"),
    ),
    (
        CATEGORY_GRAPHICS,
        include_str!("glyphs/category-graphics.svg"),
    ),
    (
        CATEGORY_INTERNET,
        include_str!("glyphs/category-internet.svg"),
    ),
    (CATEGORY_OFFICE, include_str!("glyphs/category-office.svg")),
    (CATEGORY_GAMES, include_str!("glyphs/category-games.svg")),
    (
        CATEGORY_SOFTWARE,
        include_str!("glyphs/category-software.svg"),
    ),
    (
        CATEGORY_DEVELOPMENT,
        include_str!("glyphs/category-development.svg"),
    ),
    (
        CATEGORY_EDUCATION,
        include_str!("glyphs/category-education.svg"),
    ),
    (
        CATEGORY_UTILITIES,
        include_str!("glyphs/category-utilities.svg"),
    ),
    (
        CATEGORY_WAYDROID,
        include_str!("glyphs/category-waydroid.svg"),
    ),
    (CATEGORY_OTHER, include_str!("glyphs/category-other.svg")),
    // Then the rows that stand inside a column rather than along the row.
    (CATEGORY_MUSIC, include_str!("glyphs/category-music.svg")),
    (CATEGORY_VIDEO, include_str!("glyphs/category-video.svg")),
    (CATEGORY_IMAGES, include_str!("glyphs/category-images.svg")),
    (CATEGORY_FILES, include_str!("glyphs/category-files.svg")),
    // And the rows the file explorer under it is made of.
    (FILE_FOLDER, include_str!("glyphs/file-folder.svg")),
    (FILE_PAGE, include_str!("glyphs/file-page.svg")),
    (FILE_DRIVE, include_str!("glyphs/file-drive.svg")),
    (FILE_HOME, include_str!("glyphs/file-home.svg")),
    // The Settings column's own rows, and the marks its lists are made of.
    (
        SETTING_APPEARANCE,
        include_str!("glyphs/setting-appearance.svg"),
    ),
    (SETTING_ACCENT, include_str!("glyphs/setting-accent.svg")),
    (SETTING_THEME, include_str!("glyphs/setting-theme.svg")),
    (
        SETTING_WALLPAPER,
        include_str!("glyphs/setting-wallpaper.svg"),
    ),
    (SETTING_ICONS, include_str!("glyphs/setting-icons.svg")),
    (SETTING_DISPLAY, include_str!("glyphs/setting-display.svg")),
    (
        SETTING_RESOLUTION,
        include_str!("glyphs/setting-resolution.svg"),
    ),
    (SETTING_REFRESH, include_str!("glyphs/setting-refresh.svg")),
    (
        SETTING_ORIENTATION,
        include_str!("glyphs/setting-orientation.svg"),
    ),
    (
        SETTING_ROTATION_0,
        include_str!("glyphs/setting-rotation-0.svg"),
    ),
    (
        SETTING_ROTATION_90,
        include_str!("glyphs/setting-rotation-90.svg"),
    ),
    (
        SETTING_ROTATION_180,
        include_str!("glyphs/setting-rotation-180.svg"),
    ),
    (
        SETTING_ROTATION_270,
        include_str!("glyphs/setting-rotation-270.svg"),
    ),
    (SETTING_ORDER, include_str!("glyphs/setting-order.svg")),
    (
        SETTING_NIGHT_LIGHT,
        include_str!("glyphs/setting-night-light.svg"),
    ),
    (
        SETTING_SCHEDULE,
        include_str!("glyphs/setting-schedule.svg"),
    ),
    (SETTING_HDR, include_str!("glyphs/setting-hdr.svg")),
    (
        SETTING_SCREEN_REST,
        include_str!("glyphs/setting-screen-rest.svg"),
    ),
    (
        SETTING_MICROPHONE,
        include_str!("glyphs/setting-microphone.svg"),
    ),
    (SETTING_NETWORK, include_str!("glyphs/setting-network.svg")),
    (SETTING_ADDRESS, include_str!("glyphs/setting-address.svg")),
    (
        SETTING_NAME_SERVER,
        include_str!("glyphs/setting-name-server.svg"),
    ),
    (SETTING_TYPED, include_str!("glyphs/setting-typed.svg")),
    (SETTING_CONNECT, include_str!("glyphs/setting-connect.svg")),
    (
        SETTING_DISCONNECT,
        include_str!("glyphs/setting-disconnect.svg"),
    ),
    (SETTING_WIFI, include_str!("glyphs/setting-wifi.svg")),
    (
        SETTING_ETHERNET,
        include_str!("glyphs/setting-ethernet.svg"),
    ),
    (SIGNAL_WEAK, include_str!("glyphs/signal-weak.svg")),
    (SIGNAL_FAIR, include_str!("glyphs/signal-fair.svg")),
    (SIGNAL_STRONG, include_str!("glyphs/signal-strong.svg")),
    (BATTERY_EMPTY, include_str!("glyphs/battery-empty.svg")),
    (BATTERY_LOW, include_str!("glyphs/battery-low.svg")),
    (BATTERY_HALF, include_str!("glyphs/battery-half.svg")),
    (BATTERY_HIGH, include_str!("glyphs/battery-high.svg")),
    (BATTERY_FULL, include_str!("glyphs/battery-full.svg")),
    (
        BATTERY_CHARGING,
        include_str!("glyphs/battery-charging.svg"),
    ),
    (
        SETTING_BLUETOOTH,
        include_str!("glyphs/setting-bluetooth.svg"),
    ),
    (SETTING_SYSTEM, include_str!("glyphs/setting-system.svg")),
    (SETTING_SCALE, include_str!("glyphs/setting-scale.svg")),
    (SETTING_USERS, include_str!("glyphs/setting-users.svg")),
    (SETTING_PERSON, include_str!("glyphs/setting-person.svg")),
    (
        SETTING_ADD_USER,
        include_str!("glyphs/setting-add-user.svg"),
    ),
    (SETTING_NAME, include_str!("glyphs/setting-name.svg")),
    (
        SETTING_USERNAME,
        include_str!("glyphs/setting-username.svg"),
    ),
    (
        SETTING_PASSWORD,
        include_str!("glyphs/setting-password.svg"),
    ),
    (SETTING_AVATAR, include_str!("glyphs/setting-avatar.svg")),
    (
        SETTING_ACCOUNT_TYPE,
        include_str!("glyphs/setting-account-type.svg"),
    ),
    (SETTING_PIP, include_str!("glyphs/setting-pip.svg")),
    (
        SETTING_PIP_SIZE,
        include_str!("glyphs/setting-pip-size.svg"),
    ),
    (
        SETTING_PIP_TOP_LEFT,
        include_str!("glyphs/setting-pip-top-left.svg"),
    ),
    (
        SETTING_PIP_TOP_RIGHT,
        include_str!("glyphs/setting-pip-top-right.svg"),
    ),
    (
        SETTING_PIP_BOTTOM_LEFT,
        include_str!("glyphs/setting-pip-bottom-left.svg"),
    ),
    (
        SETTING_PIP_BOTTOM_RIGHT,
        include_str!("glyphs/setting-pip-bottom-right.svg"),
    ),
    (SETTING_INFO, include_str!("glyphs/setting-info.svg")),
    (SWATCH, include_str!("glyphs/swatch.svg")),
    (CHOSEN, include_str!("glyphs/chosen.svg")),
    (ADD, include_str!("glyphs/add.svg")),
    // The context menu's own rows.
    (UNINSTALL, include_str!("glyphs/uninstall.svg")),
    (LAUNCH, include_str!("glyphs/launch.svg")),
    (OPEN_WITH, include_str!("glyphs/open-with.svg")),
    (SORT, include_str!("glyphs/sort.svg")),
    (COPY, include_str!("glyphs/copy.svg")),
    (MOVE, include_str!("glyphs/move.svg")),
    (PASTE, include_str!("glyphs/paste.svg")),
    (RENAME, include_str!("glyphs/rename.svg")),
    (NEW_FOLDER, include_str!("glyphs/new-folder.svg")),
    (TRASH_EMPTY, include_str!("glyphs/trash-empty.svg")),
    (SELECT_MULTIPLE, include_str!("glyphs/select-multiple.svg")),
    (SCREENSHOT, include_str!("glyphs/screenshot.svg")),
    // The rows a column of the user's own files carries above the files.
    (SEARCH, include_str!("glyphs/search.svg")),
    (SEARCH_CLEAR, include_str!("glyphs/search-clear.svg")),
    (AUTHENTICATE, include_str!("glyphs/authenticate.svg")),
    // The Steam column, and the rows that came out of it.
    (CATEGORY_STEAM, include_str!("glyphs/category-steam.svg")),
    (STEAM, include_str!("glyphs/steam.svg")),
    (REFRESH, include_str!("glyphs/refresh.svg")),
    (SIGN_OUT, include_str!("glyphs/sign-out.svg")),
    // The shell's own mark, which is about none of the above.
    (LOGO, include_str!("glyphs/logo.svg")),
];

/// A decoded icon, always square RGBA8.
pub struct Icon {
    pub size: u32,
    pub rgba: Vec<u8>,
}

impl Icon {
    /// Rasterise a drawing the shell carries itself.
    ///
    /// The glyphs on the quick-settings bars come through here rather than out
    /// of the icon theme. A theme is something a desktop installs, and the
    /// point of those bars is a session with no desktop in it — a speaker that
    /// is missing on a machine with only hicolor installed would leave the
    /// volume row unlabelled on exactly the systems this is for.
    pub fn builtin(svg: &str, size: u32) -> Option<Self> {
        Some(Icon {
            size,
            rgba: rasterise_svg(svg.as_bytes(), None, size)?,
        })
    }

    /// Build one out of pixels somebody handed over rather than out of a file.
    ///
    /// For the announcement that carries its picture as raw bytes — album art,
    /// a correspondent's face — which arrives as a block of samples and a
    /// description of how to read it, and never as anything nameable.
    ///
    /// `stride` is the distance from one row of that block to the next, which
    /// is not always the width: a sender is free to pad its rows, and reading
    /// the block as though it were tight is what turns a padded picture into a
    /// diagonal smear. `channels` is three or four, and three means every
    /// pixel is opaque.
    ///
    /// Scaled to `size` the same way a file is, so a picture of any shape ends
    /// up square without being stretched into one.
    pub fn from_pixels(
        width: u32,
        height: u32,
        stride: u32,
        channels: u32,
        data: &[u8],
        size: u32,
    ) -> Option<Self> {
        if width == 0 || height == 0 || size == 0 || !(3..=4).contains(&channels) {
            return None;
        }
        let row = width.checked_mul(channels)?;
        if stride < row {
            return None;
        }
        // The last row need only be as long as the picture is wide: a sender
        // that padded every row *between* its rows has no reason to pad past
        // the end, and refusing that would be refusing a valid picture.
        let needed = stride
            .checked_mul(height.checked_sub(1)?)?
            .checked_add(row)?;
        if data.len() < needed as usize {
            return None;
        }

        let mut rgba = Vec::with_capacity((width * height * 4) as usize);
        for y in 0..height {
            let start = (y * stride) as usize;
            for x in 0..width {
                let pixel = start + (x * channels) as usize;
                rgba.extend_from_slice(&data[pixel..pixel + 3]);
                rgba.push(if channels == 4 { data[pixel + 3] } else { 255 });
            }
        }

        let buffer = image::RgbaImage::from_raw(width, height, rgba)?;
        Some(Icon {
            size,
            rgba: fit_raster(image::DynamicImage::ImageRgba8(buffer), size),
        })
    }
}

pub struct IconLoader {
    /// Every directory that can hold an icon, in priority order, enumerated
    /// once at construction.
    ///
    /// The alternative — walking the theme tree per icon name — costs a couple
    /// of thousand `stat` calls each time, and a launcher resolves a hundred
    /// names before it can draw anything.
    search_dirs: Vec<(u32, PathBuf)>,
    cache: HashMap<String, Option<PathBuf>>,
}

impl IconLoader {
    pub fn new() -> Self {
        let roots = icon_roots();
        let theme = current_theme();
        let pixmaps = ["/usr/share/pixmaps", "/usr/local/share/pixmaps"]
            .into_iter()
            .map(PathBuf::from)
            .collect();
        Self::from_paths(roots, theme, pixmaps)
    }

    fn from_paths(roots: Vec<PathBuf>, theme: String, pixmap_dirs: Vec<PathBuf>) -> Self {
        let mut themes = Vec::new();
        collect_theme_chain(&roots, &theme, &mut themes, &mut HashSet::new());

        // hicolor is the mandated final fallback for every theme.
        if !themes.iter().any(|t| t == "hicolor") {
            themes.push("hicolor".to_string());
        }

        // Rank encodes the priority order: earlier themes beat later ones, and
        // within a theme a larger (or scalable) source beats a smaller one.
        let mut search_dirs: Vec<(u32, PathBuf)> = Vec::new();
        let mut theme_count = 0u32;
        let mut seen_dirs = HashSet::new();

        for (index, theme) in themes.iter().enumerate() {
            // Themes are strictly ordered, so keep their bands far apart.
            let theme_rank = (themes.len() - index) as u32 * 1_000_000;
            for root in &roots {
                let theme_dir = root.join(theme);
                if !theme_dir.is_dir() {
                    continue;
                }
                theme_count += 1;
                collect_icon_directories(
                    &theme_dir,
                    &theme_dir,
                    theme_rank,
                    0,
                    &mut seen_dirs,
                    &mut search_dirs,
                );
            }
        }

        // Pixmaps are the last resort, below every theme.
        for dir in pixmap_dirs {
            if dir.is_dir() {
                search_dirs.push((0, dir));
            }
        }

        // Highest rank first, so candidate resolution prefers it.
        search_dirs.sort_by_key(|entry| std::cmp::Reverse(entry.0));

        tracing::debug!(
            ?themes,
            themes_found = theme_count,
            dirs = search_dirs.len(),
            "icon search path"
        );

        Self {
            search_dirs,
            cache: HashMap::new(),
        }
    }

    /// Load and rasterise an icon to exactly `size` x `size` RGBA pixels.
    pub fn load(&mut self, name: &str, size: u32) -> Option<Icon> {
        if name.is_empty() || size == 0 {
            return None;
        }

        if matches!(self.cache.get(name), Some(None)) {
            return None;
        }

        let cached = self.cache.get(name).and_then(Clone::clone);
        let mut tried = HashSet::new();
        let candidates = cached
            .into_iter()
            .chain(self.candidates(name))
            .filter(|path| tried.insert(path.clone()));

        for path in candidates {
            if !path.is_file() {
                continue;
            }
            if let Some(icon) = load_path(&path, size) {
                self.cache.insert(name.to_string(), Some(path));
                return Some(icon);
            }
            // Do not let a broken SVG or unsupported XPM in the current theme
            // hide a valid PNG/SVG inherited from a lower-priority theme.
            tracing::debug!(icon = name, path = %path.display(), "could not decode icon; trying fallback");
        }

        self.cache.insert(name.to_string(), None);
        tracing::debug!(icon = name, "icon not found or no decodable fallback");
        None
    }

    /// The same icon, measured as the *shape* of itself rather than loaded as a
    /// picture of one.
    ///
    /// For an application that asked for the shell's own material — see
    /// [`crate::apps::App::wears_shell_material`]. The drawing is rasterised at
    /// [`SDF_SUPERSAMPLE`] times the cell, thresholded at half alpha into a
    /// silhouette, and measured by the same transform every built-in mark goes
    /// through, so what comes back is the same kind of thing as a glyph and the
    /// shader treats it as one.
    ///
    /// Thresholded coverage rather than the drawing's own geometry, which is
    /// what lets this take a `.png` as happily as a `.svg`: an icon theme holds
    /// both, an application does not choose which of its files the shell finds,
    /// and half alpha is where a rasteriser's own edge sits.
    ///
    /// `None` where the icon could not be found or could not be measured, and
    /// the caller must then fall back to [`Self::load`] rather than registering
    /// the name — a name registered without a field behind it is a picture the
    /// shader reads as a measurement.
    pub fn load_shape(&mut self, name: &str, size: u32) -> Option<Icon> {
        let fine = size.checked_mul(SDF_SUPERSAMPLE)?;
        let coverage = self.load(name, fine)?;
        let inside: Vec<bool> = coverage
            .rgba
            .chunks_exact(4)
            .map(|px| px[3] >= 128)
            .collect();
        distance_field(&inside, fine, size)
    }

    fn candidates(&self, name: &str) -> Vec<PathBuf> {
        let path = Path::new(name);
        if path.is_absolute() {
            return vec![path.to_path_buf()];
        }

        let Some(file_name) = path.file_name().and_then(|value| value.to_str()) else {
            return Vec::new();
        };
        let supplied_extension = known_icon_extension(path);
        let stem = supplied_extension
            .and_then(|_| path.file_stem())
            .and_then(|value| value.to_str())
            .unwrap_or(file_name);

        let mut candidates = Vec::new();
        for (_, dir) in &self.search_dirs {
            // An explicit recognised suffix gets first chance, but decoding
            // failure still falls through to the other supported formats.
            if supplied_extension.is_some() {
                candidates.push(dir.join(file_name));
            }
            for extension in ICON_EXTENSIONS {
                if supplied_extension == Some(extension) {
                    continue;
                }
                candidates.push(dir.join(format!("{stem}.{extension}")));
            }
        }
        candidates
    }
}

impl Default for IconLoader {
    fn default() -> Self {
        Self::new()
    }
}

fn load_path(path: &Path, size: u32) -> Option<Icon> {
    if size == 0 {
        return None;
    }

    // Read once and sniff the contents.  AppImage-generated desktop entries
    // commonly use absolute PNG/SVG paths with no filename extension.
    let data = std::fs::read(path).ok()?;
    let rgba = if let Some(svg) = rasterise_svg(&data, path.parent(), size) {
        svg
    } else {
        let image = image::load_from_memory(&data).ok()?;
        fit_raster(image, size)
    };
    Some(Icon { size, rgba })
}

/// The word a glyph writes in its own source to say that it ships as the
/// *shape* of a mark rather than as a picture of one.
///
/// A glyph that carries it is rasterised into a signed distance field by
/// [`builtin_distance_field`] and shaded by the quad shader — see
/// `glyph_material` in shaders.wgsl. One that does not is rasterised as it was
/// drawn, and the atlas hands the shader its pixels.
///
/// In the drawing rather than in a list here, because the two facts are the
/// same fact: a file that paints no rim, no ridge and no sheen *is* a shape,
/// and a list would be a second place for that to be true or false. The whole
/// set is meant to end up on this side of the line; until it does, which glyph
/// is which is written where a person editing one can see it.
pub const SHAPE_MARK: &str = "lxb:shape";

/// Whether this drawing is a shape to be shaded rather than a picture to be
/// sampled. See [`SHAPE_MARK`].
pub fn is_shape(drawing: &str) -> bool {
    drawing.contains(SHAPE_MARK)
}

/// The same question by name, for a layout deciding whether the quad it is
/// about to push wants the material — [`crate::ui`] cannot see the drawing,
/// only what it is called.
pub fn shaped(name: &str) -> bool {
    // Everything cut from the shell's own type is a measurement by
    // construction, whether it is a digit of the clock or a heading of an
    // index. See [`LETTER_PREFIX`].
    if name.starts_with(LETTER_PREFIX) {
        return true;
    }
    static SHAPES: std::sync::OnceLock<Vec<&'static str>> = std::sync::OnceLock::new();
    if SHAPES
        .get_or_init(|| {
            BUILTIN
                .iter()
                .filter(|(_, drawing)| is_shape(drawing))
                .map(|(name, _)| *name)
                .collect()
        })
        .contains(&name)
    {
        return true;
    }
    // And the marks that arrived with an integration package, which are shapes
    // on exactly the same terms.
    if package_glyphs()
        .iter()
        .any(|(had, drawing)| had == name && is_shape(drawing))
    {
        return true;
    }
    // And an application's own icon, where the application asked for it and the
    // atlas was able to measure it. See [`remember_shaped_icon`].
    SHAPED_ICONS
        .read()
        .is_ok_and(|shaped| shaped.iter().any(|had| had == name))
}

/// The icons out of the theme that are being drawn as shapes rather than as
/// pictures, by the name they were looked up under.
///
/// Not a `lxb:` name, which is the whole reason this list exists: everything
/// else the shader cuts glass to is one of the shell's own marks and answers to
/// its own namespace, and these are somebody else's file in somebody else's
/// theme. What makes one of them a shape is that its `.desktop` file asked —
/// see [`crate::apps::App::wears_shell_material`] — and the *name* is all
/// [`shaped`] gets, so the answer has to be written down where a name can reach
/// it.
///
/// A lock rather than a `OnceLock`, because this is filled as the atlas is
/// built and the atlas is built more than once: a session that rescans what is
/// installed measures whatever it found again. It only ever grows, and it is a
/// handful of short strings — a machine with one such application has one
/// entry.
static SHAPED_ICONS: std::sync::RwLock<Vec<String>> = std::sync::RwLock::new(Vec::new());

/// Write down that this icon has been measured into a field, so that every
/// quad carrying it is given the material.
///
/// Called by whoever put the measurement in the atlas, and only where that
/// succeeded: a name registered without a field behind it is a picture the
/// shader would read as a distance field, which comes out as a pale smear. See
/// [`crate::icons::shape_of`].
pub fn remember_shaped_icon(name: &str) {
    let Ok(mut shaped) = SHAPED_ICONS.write() else {
        return;
    };
    if shaped.iter().any(|had| had == name) {
        return;
    }
    tracing::debug!(icon = name, "an application asked for the shell's material");
    shaped.push(name.to_string());
}

/// Where a package that is not the shell's own puts a mark of its own, under
/// each of the XDG data directories.
///
/// One directory and one rule: an integration ships `lxb/glyphs/<name>.svg`,
/// and the shell draws it as `lxb:<name>`. There is no manifest, no
/// registration and nothing in the shell that names the file — a package is
/// installed or it is not, and the mark is there or it is not.
///
/// This exists because the shell is one binary and its integrations are not.
/// `lxb-retroarch` is a package a machine may not have; the mark every row of
/// that column wears has to arrive with it rather than being compiled into a
/// shell that would draw it on nine machines out of ten that never emulate
/// anything. See [`BUILTIN`], which is the other half of the argument: what
/// the shell needs *whatever* is installed is compiled in, and this is
/// deliberately not that.
pub const PACKAGE_GLYPHS: &str = "lxb/glyphs";

/// Every mark an installed integration brought with it, as `(name, drawing)`.
///
/// Read from the disk once and held, because three separate things ask for it
/// — the atlas that rasterises them, [`shaped`] on every quad that carries
/// one, and the integration itself asking whether its own mark is here at all
/// — and a set of marks that changed under a running shell would be a row
/// whose material changed while somebody was looking at it.
pub fn package_glyphs() -> &'static [(String, String)] {
    static FOUND: std::sync::OnceLock<Vec<(String, String)>> = std::sync::OnceLock::new();
    FOUND.get_or_init(from_packages)
}

/// The same, read fresh. [`package_glyphs`] is what everything else calls.
///
/// Most specific directory first, and the first spelling of a name wins — the
/// order [`crate::xdg_data_dirs`] answers in, so a mark under the user's own
/// data directory stands in front of the package's. A name one of the shell's
/// own marks already has is refused outright: a package may add to this set and
/// may not redraw it, or an installed integration could change what the volume
/// bar looks like.
fn from_packages() -> Vec<(String, String)> {
    let mut found: Vec<(String, String)> = Vec::new();
    for dir in crate::xdg_data_dirs(PACKAGE_GLYPHS) {
        let Ok(entries) = std::fs::read_dir(&dir) else {
            continue;
        };
        let mut here: Vec<std::path::PathBuf> =
            entries.flatten().map(|entry| entry.path()).collect();
        // A directory listing is in whatever order the filesystem likes, and
        // the atlas's slots are handed out in the order they arrive.
        here.sort();
        for path in here {
            if path.extension().and_then(|kind| kind.to_str()) != Some("svg") {
                continue;
            }
            let Some(stem) = path.file_stem().and_then(|stem| stem.to_str()) else {
                continue;
            };
            // `lxb:` is the shell's own namespace and the package's marks are
            // in it: what a name says is that this is a mark of the interface
            // rather than an icon out of a theme, which is as true of one that
            // arrived with an integration as of one compiled in.
            let name = format!("lxb:{stem}");
            if BUILTIN.iter().any(|(had, _)| *had == name) {
                tracing::warn!(
                    glyph = %name,
                    at = %path.display(),
                    "a package tried to redraw one of the shell's own marks"
                );
                continue;
            }
            if found.iter().any(|(had, _)| *had == name) {
                continue;
            }
            match std::fs::read_to_string(&path) {
                Ok(drawing) => {
                    tracing::info!(glyph = %name, at = %path.display(), "a package brought a mark");
                    found.push((name, drawing));
                }
                Err(err) => tracing::warn!(at = %path.display(), ?err, "could not read a mark"),
            }
        }
    }
    found
}

/// Half the range a glyph's distance field spans, as a fraction of the cell.
///
/// The field is stored in eight bits of alpha, so range and precision trade
/// against each other: this is a quarter of the cell end to end, which is
/// wider than any bevel wants and still resolves an eighth of a pixel at the
/// size a category is drawn. The shader undoes it with the same constant.
pub const SDF_RANGE: f32 = 0.125;

/// How much finer than the cell the shape is measured before the field is
/// reduced to it. Distance is a continuous quantity being sampled on a grid,
/// and a grid the size of the cell can only ever answer in whole pixels —
/// which comes out of the shader as a bevel with steps in it.
pub const SDF_SUPERSAMPLE: u32 = 4;

/// Rasterise a glyph as a *measurement of its shape* rather than as a picture:
/// alpha carries how far each pixel is from the nearest edge, negative inside.
///
/// This is what lets the quad shader treat a glyph as a slab of glass — see
/// `fs_quad` in shaders.wgsl. Colour is left white throughout; nothing samples
/// it, and white is what the multiply expects if anything ever does.
pub fn builtin_distance_field(drawing: &str, size: u32) -> Option<Icon> {
    let fine = size.checked_mul(SDF_SUPERSAMPLE)?;
    let coverage = rasterise_svg(drawing.as_bytes(), None, fine)?;
    let inside: Vec<bool> = coverage.chunks_exact(4).map(|px| px[3] >= 128).collect();
    distance_field(&inside, fine, size)
}

/// The same measurement, taken of coverage somebody else rasterised.
///
/// `inside` is a `fine`-by-`fine` grid of whether each pixel is within the
/// shape, and `fine` must be [`SDF_SUPERSAMPLE`] times `size` — see
/// [`builtin_distance_field`], which is this with a drawing on the front of it.
/// The other caller is the shell's own type: the corner's letters are cut out of
/// the bundled font and measured here, so a letter and a glyph are the same kind
/// of thing to the shader and there is one transform rather than two.
pub fn distance_field(inside: &[bool], fine: u32, size: u32) -> Option<Icon> {
    if inside.len() != (fine as usize).pow(2) || fine != size.checked_mul(SDF_SUPERSAMPLE)? {
        return None;
    }

    // Two transforms: how far each pixel outside the shape is from it, and how
    // far each pixel inside it is from getting out. Their difference is the
    // signed field, and it crosses zero on the boundary between the two.
    let out = euclidean_distance(inside, fine, false);
    let within = euclidean_distance(inside, fine, true);

    let block = SDF_SUPERSAMPLE as usize;
    let cell = size as usize;
    let mut rgba = vec![255u8; cell * cell * 4];
    for y in 0..cell {
        for x in 0..cell {
            // The mean over the block the output pixel covers. A distance
            // field is smooth, so averaging it is a reduction rather than the
            // aliasing the same average would be on a picture.
            let mut sum = 0.0f32;
            for dy in 0..block {
                for dx in 0..block {
                    let i = (y * block + dy) * fine as usize + x * block + dx;
                    sum += out[i] - within[i];
                }
            }
            let fine_px = sum / (block * block) as f32;
            // Into fractions of the cell, then into the stored range.
            let cell_fraction = fine_px / fine as f32;
            let stored = 0.5 + cell_fraction / (2.0 * SDF_RANGE);
            rgba[(y * cell + x) * 4 + 3] = (stored.clamp(0.0, 1.0) * 255.0).round() as u8;
        }
    }
    Some(Icon { size, rgba })
}

/// Exact Euclidean distance to the nearest pixel of the given kind, by
/// Felzenszwalb and Huttenlocher's two-pass transform: the lower envelope of
/// one parabola per seed, taken along the columns and then along the rows.
///
/// Exact rather than the usual chamfer approximation because the error in a
/// chamfer field is largest along the diagonals, and a bevel computed from it
/// has visible flats at forty-five degrees.
fn euclidean_distance(inside: &[bool], size: u32, seed_outside: bool) -> Vec<f32> {
    let n = size as usize;
    let far = f32::MAX / 4.0;
    let mut grid: Vec<f32> = inside
        .iter()
        .map(|&i| if i == seed_outside { far } else { 0.0 })
        .collect();

    let mut line = vec![0.0f32; n];
    for x in 0..n {
        for y in 0..n {
            line[y] = grid[y * n + x];
        }
        let done = envelope(&line);
        for y in 0..n {
            grid[y * n + x] = done[y];
        }
    }
    for y in 0..n {
        let done = envelope(&grid[y * n..(y + 1) * n]);
        grid[y * n..(y + 1) * n].copy_from_slice(&done);
    }
    grid.iter().map(|d| d.max(0.0).sqrt()).collect()
}

/// The lower envelope of the parabolas `f[q] + (x - q)^2`, sampled back onto
/// the same grid. One dimension of the transform above.
fn envelope(f: &[f32]) -> Vec<f32> {
    let n = f.len();
    let mut out = vec![0.0f32; n];
    if n == 0 {
        return out;
    }
    let mut vertex = vec![0usize; n];
    let mut cross = vec![0.0f32; n + 1];
    let mut k = 0usize;
    cross[0] = f32::MIN;
    cross[1] = f32::MAX;
    let sq = |v: usize| (v * v) as f32;

    for q in 1..n {
        loop {
            let s = ((f[q] + sq(q)) - (f[vertex[k]] + sq(vertex[k])))
                / (2.0 * q as f32 - 2.0 * vertex[k] as f32);
            if s <= cross[k] && k > 0 {
                k -= 1;
            } else {
                k += 1;
                vertex[k] = q;
                cross[k] = s;
                cross[k + 1] = f32::MAX;
                break;
            }
        }
    }

    k = 0;
    for (q, slot) in out.iter_mut().enumerate() {
        while cross[k + 1] < q as f32 {
            k += 1;
        }
        *slot = (q as f32 - vertex[k] as f32).powi(2) + f[vertex[k]];
    }
    out
}

pub fn rasterise_svg(data: &[u8], resources_dir: Option<&Path>, size: u32) -> Option<Vec<u8>> {
    use resvg::tiny_skia;
    use resvg::usvg;

    // resvg is built without its text feature: icons are shapes, and pulling in
    // a font database here would duplicate the one glyphon already owns.
    let options = usvg::Options {
        resources_dir: resources_dir.map(Path::to_path_buf),
        ..Default::default()
    };
    let tree = usvg::Tree::from_data(data, &options).ok()?;

    let mut pixmap = tiny_skia::Pixmap::new(size, size)?;
    let tree_size = tree.size();
    // Fit the drawing into the square without distorting it.
    let scale = (size as f32 / tree_size.width()).min(size as f32 / tree_size.height());
    let dx = (size as f32 - tree_size.width() * scale) / 2.0;
    let dy = (size as f32 - tree_size.height() * scale) / 2.0;

    let transform = tiny_skia::Transform::from_translate(dx, dy).pre_scale(scale, scale);
    resvg::render(&tree, transform, &mut pixmap.as_mut());

    let mut rgba = pixmap.take();
    // tiny-skia stores premultiplied RGBA, while the atlas pipeline uses
    // straight-alpha blending.  Convert once here to avoid dark translucent
    // edges and double-multiplying SVG alpha in the shader pipeline.
    unpremultiply_rgba(&mut rgba);
    Some(rgba)
}

fn fit_raster(image: image::DynamicImage, size: u32) -> Vec<u8> {
    let resized = image
        .resize(size, size, image::imageops::FilterType::Lanczos3)
        .to_rgba8();
    if resized.width() == size && resized.height() == size {
        return resized.into_raw();
    }

    let mut square = image::RgbaImage::new(size, size);
    let x = (size - resized.width()) / 2;
    let y = (size - resized.height()) / 2;
    image::imageops::overlay(&mut square, &resized, i64::from(x), i64::from(y));
    square.into_raw()
}

fn unpremultiply_rgba(rgba: &mut [u8]) {
    for pixel in rgba.chunks_exact_mut(4) {
        let alpha = u32::from(pixel[3]);
        if alpha == 0 || alpha == 255 {
            continue;
        }
        for channel in &mut pixel[..3] {
            let straight = (u32::from(*channel) * 255 + alpha / 2) / alpha;
            *channel = straight.min(255) as u8;
        }
    }
}

fn known_icon_extension(path: &Path) -> Option<&'static str> {
    let extension = path.extension()?.to_str()?;
    ICON_EXTENSIONS
        .into_iter()
        .find(|known| extension.eq_ignore_ascii_case(known))
}

/// Discover directories recursively instead of assuming either
/// `size/context` or `context/size`.  Both layouts are common in real themes,
/// and the specification permits arbitrary relative paths in `Directories=`.
fn collect_icon_directories(
    theme_root: &Path,
    directory: &Path,
    theme_rank: u32,
    depth: usize,
    seen: &mut HashSet<PathBuf>,
    out: &mut Vec<(u32, PathBuf)>,
) {
    if depth > MAX_THEME_DEPTH {
        return;
    }

    let identity = directory
        .canonicalize()
        .unwrap_or_else(|_| directory.to_path_buf());
    if !seen.insert(identity) {
        return;
    }

    let Ok(entries) = std::fs::read_dir(directory) else {
        return;
    };
    let mut contains_icons = false;
    let mut children = Vec::new();

    for entry in entries.flatten() {
        let path = entry.path();
        if path.is_dir() {
            children.push(path);
        } else if known_icon_extension(&path).is_some() {
            contains_icons = true;
        }
    }

    if contains_icons {
        let relative = directory.strip_prefix(theme_root).unwrap_or(directory);
        out.push((
            theme_rank + directory_rank(relative),
            directory.to_path_buf(),
        ));
    }
    for child in children {
        collect_icon_directories(theme_root, &child, theme_rank, depth + 1, seen, out);
    }
}

/// Rank a theme size directory so the closest-to-ideal resolution wins.
///
/// Scalable directories rank highest, then the largest fixed size.
fn directory_rank(path: &Path) -> u32 {
    let mut largest = 0;
    for component in path
        .components()
        .filter_map(|part| part.as_os_str().to_str())
    {
        if component.to_ascii_lowercase().contains("scalable") {
            return 100_000;
        }

        // Components are commonly `48x48`, `48`, or `48x48@2`.
        let dims = component.split('@').next().unwrap_or(component);
        let size = dims
            .split(['x', 'X'])
            .next()
            .and_then(|value| value.parse::<u32>().ok())
            .unwrap_or(0);
        largest = largest.max(size.min(1024));
    }
    // Prefer bigger sources; the decoder always scales down to the atlas cell.
    largest
}

fn icon_roots() -> Vec<PathBuf> {
    // `~/.icons` is legacy but still widely populated, so it comes first and
    // is not covered by the XDG data dirs.
    let mut roots = Vec::new();
    if let Some(home) = std::env::var_os("HOME") {
        roots.push(PathBuf::from(&home).join(".icons"));
    }
    roots.extend(crate::xdg_data_dirs("icons"));

    roots.retain(|r| r.is_dir());
    let mut seen = HashSet::new();
    roots.retain(|root| seen.insert(root.clone()));
    roots
}

/// Best guess at the user's theme, since we have no settings daemon to ask.
fn current_theme() -> String {
    if let Ok(theme) = std::env::var("LXB_ICON_THEME") {
        if !theme.is_empty() {
            return theme;
        }
    }
    // KDE records it here; GTK has its own file. Reading them is cheap.
    //
    // Both, now. This said the same and read only KDE's, which meant a machine
    // configured through GTK alone — GNOME, XFCE, or a bare session where the
    // user has set a theme and nothing else — fell all the way back to
    // hicolor. That is survivable for application icons, because a program
    // installs its own into hicolor, and it is not survivable for the standard
    // names: `software-update-available` and its like come *from* a theme, and
    // hicolor is exactly the theme that does not carry them. An announcement
    // naming one got the bell.
    if let Some(theme) = read_kde_theme().or_else(read_gtk_theme) {
        return theme;
    }
    // And when neither desktop is here to have left a file — which is the
    // machine this shell is *for*, a console with no desktop on it at all —
    // the shell picks one itself rather than falling to hicolor.
    //
    // Falling to hicolor is not a neutral default, it is a broken one. hicolor
    // is the place a program installs its *own* icon, so an application still
    // has a picture there; what it has never carried is the standard names —
    // `software-update-available`, `dialog-warning`, `network-wireless` — and
    // those are exactly what a program names when it announces something. A
    // session with no desktop would have had a bell on every announcement,
    // with a perfectly good theme sitting installed on the disk unread.
    //
    // Only when nothing has been configured, so a user who has said what they
    // want is never second-guessed, and it costs nothing on a machine that has
    // said: this does not run at all until both files have come back empty.
    if let Some(theme) = any_installed_theme(&icon_roots()) {
        tracing::info!(theme, "no desktop has named an icon theme; using this one");
        return theme;
    }
    "hicolor".to_string()
}

/// The themes most likely to be both installed and complete, in the order they
/// are worth trying.
///
/// The two that ship with the two toolkits: a machine with any GTK application
/// on it usually has Adwaita, and one with any Qt application usually has
/// Breeze. Papirus after them because it is the one people install on purpose.
///
/// A list of names is a blunt instrument and it is the honest one here. The
/// alternative is to rank what is installed by how many icons it carries,
/// which means walking every theme on the disk to answer a question asked once
/// per session, and still gets it wrong — the biggest theme is not the most
/// complete one, it is the one with the most sizes.
const LIKELY_THEMES: [&str; 3] = ["Adwaita", "breeze", "Papirus"];

/// A theme that is actually on this machine, when nothing has said which to
/// use.
///
/// Anything with an `index.theme` that lists directories. That last part is
/// what keeps cursor themes out: a cursor theme is an icon theme by file
/// layout — same place on disk, same index file — and carries no icons at all,
/// so a session that picked one would be back to having none.
fn any_installed_theme(roots: &[PathBuf]) -> Option<String> {
    let mut found: Vec<String> = Vec::new();
    for root in roots {
        let Ok(entries) = std::fs::read_dir(root) else {
            continue;
        };
        for entry in entries.flatten() {
            let Some(name) = entry.file_name().to_str().map(str::to_string) else {
                continue;
            };
            if name == "hicolor" || found.contains(&name) {
                continue;
            }
            if read_ini_key(
                &entry.path().join("index.theme"),
                Some("[Icon Theme]"),
                "Directories",
            )
            .is_some()
            {
                found.push(name);
            }
        }
    }

    LIKELY_THEMES
        .into_iter()
        .find(|likely| found.iter().any(|name| name == likely))
        .map(str::to_string)
        // Failing all of those, whatever is here — sorted, so that two
        // sessions on one machine make the same choice and the shell does not
        // change its look because a directory was read back in another order.
        .or_else(|| {
            found.sort();
            found.into_iter().next()
        })
}

fn read_kde_theme() -> Option<String> {
    let home = PathBuf::from(std::env::var_os("HOME")?);
    read_ini_key(&home.join(".config/kdeglobals"), Some("[Icons]"), "Theme")
}

/// GTK 4 first and then GTK 3, so a machine that has moved on is read as it is
/// now rather than as it was.
fn read_gtk_theme() -> Option<String> {
    let home = PathBuf::from(std::env::var_os("HOME")?);
    ["gtk-4.0", "gtk-3.0"].into_iter().find_map(|version| {
        read_ini_key(
            &home.join(format!(".config/{version}/settings.ini")),
            Some("[Settings]"),
            "gtk-icon-theme-name",
        )
    })
}

/// One `Key=Value` out of a desktop-style ini file.
///
/// `section` is the header the key has to be under, or `None` for a file with
/// no sections. GTK's own `settings.ini` is supposed to have a `[Settings]`
/// header and is routinely written without one, so a file whose first line is
/// already a key is read as though the header it was missing had been there —
/// anything else would be refusing to read a file the toolkit that owns it
/// reads happily.
///
/// Takes the file rather than finding it, so that what it does can be tested
/// without a test setting `HOME` — an environment variable is one per process,
/// and tests here run beside each other.
fn read_ini_key(path: &Path, section: Option<&str>, key: &str) -> Option<String> {
    let raw = std::fs::read_to_string(path).ok()?;

    let mut inside = section.is_none();
    let mut seen_any_section = false;
    for line in raw.lines() {
        let line = line.trim();
        if line.starts_with('[') {
            seen_any_section = true;
            inside = section.is_none_or(|wanted| line == wanted);
            continue;
        }
        if !inside && seen_any_section {
            continue;
        }
        if let Some(value) = line.strip_prefix(key).and_then(|rest| {
            // The key and nothing longer that starts with it, and the equals
            // sign may have spaces round it as GTK writes them.
            rest.trim_start().strip_prefix('=')
        }) {
            let value = value.trim();
            if !value.is_empty() {
                return Some(value.to_string());
            }
        }
    }
    None
}

/// Walk `Inherits=` chains so a theme's fallbacks are searched too.
fn collect_theme_chain(
    roots: &[PathBuf],
    theme: &str,
    out: &mut Vec<String>,
    seen: &mut HashSet<String>,
) {
    if !seen.insert(theme.to_string()) {
        return;
    }
    out.push(theme.to_string());

    for root in roots {
        let index = root.join(theme).join("index.theme");
        let Ok(raw) = std::fs::read_to_string(&index) else {
            continue;
        };
        for line in raw.lines() {
            let line = line.trim();
            if let Some(value) = line.strip_prefix("Inherits=") {
                for parent in value.split(',').map(str::trim).filter(|p| !p.is_empty()) {
                    collect_theme_chain(roots, parent, out, seen);
                }
                return;
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::io::Cursor;
    use std::sync::atomic::{AtomicU64, Ordering};

    struct TestTree(PathBuf);

    impl TestTree {
        fn new(label: &str) -> Self {
            static NEXT: AtomicU64 = AtomicU64::new(0);
            let path = std::env::temp_dir().join(format!(
                "lxb-icon-test-{label}-{}-{}",
                std::process::id(),
                NEXT.fetch_add(1, Ordering::Relaxed)
            ));
            std::fs::create_dir_all(&path).unwrap();
            Self(path)
        }

        fn join(&self, path: impl AsRef<Path>) -> PathBuf {
            self.0.join(path)
        }
    }

    impl Drop for TestTree {
        fn drop(&mut self) {
            let _ = std::fs::remove_dir_all(&self.0);
        }
    }

    fn png_bytes(width: u32, height: u32, color: [u8; 4]) -> Vec<u8> {
        let image = image::RgbaImage::from_pixel(width, height, image::Rgba(color));
        let mut bytes = Cursor::new(Vec::new());
        image::DynamicImage::ImageRgba8(image)
            .write_to(&mut bytes, image::ImageFormat::Png)
            .unwrap();
        bytes.into_inner()
    }

    fn write(path: &Path, bytes: impl AsRef<[u8]>) {
        std::fs::create_dir_all(path.parent().unwrap()).unwrap();
        std::fs::write(path, bytes).unwrap();
    }

    fn loader_with_dirs(dirs: Vec<PathBuf>) -> IconLoader {
        IconLoader {
            search_dirs: dirs
                .into_iter()
                .enumerate()
                .map(|(index, path)| (10_000 - index as u32, path))
                .collect(),
            cache: HashMap::new(),
        }
    }

    /// The icon theme is read from whichever file the machine happens to keep
    /// it in, and read the way the toolkits that own those files write them.
    ///
    /// This decides whether a *name* resolves to anything at all. Only KDE's
    /// file was read, so a machine configured through GTK alone fell back to
    /// hicolor — survivable for application icons, which programs install into
    /// hicolor themselves, and not survivable for the standard names an
    /// announcement gives, which come from a theme and are the one thing
    /// hicolor does not carry.
    #[test]
    fn the_icon_theme_is_read_from_either_desktops_file() {
        let tree = TestTree::new("theme");

        let kde = tree.join("kdeglobals");
        write(
            &kde,
            "[General]\nTheme=NotThisOne\n\n[Icons]\nTheme=Tela-dark\n",
        );
        assert_eq!(
            read_ini_key(&kde, Some("[Icons]"), "Theme").as_deref(),
            Some("Tela-dark"),
            "the key under the section that was asked for, not the first that matches"
        );

        // GTK writes spaces round the equals and does not always write the
        // header at all.
        let gtk = tree.join("settings.ini");
        write(&gtk, "[Settings]\ngtk-icon-theme-name = Papirus\n");
        assert_eq!(
            read_ini_key(&gtk, Some("[Settings]"), "gtk-icon-theme-name").as_deref(),
            Some("Papirus")
        );
        write(&gtk, "gtk-icon-theme-name=Papirus\n");
        assert_eq!(
            read_ini_key(&gtk, Some("[Settings]"), "gtk-icon-theme-name").as_deref(),
            Some("Papirus"),
            "a headerless file is read the way GTK itself reads it"
        );

        // A key that merely starts with the one wanted is a different key.
        write(&gtk, "[Settings]\ngtk-icon-theme-name-fallback=Wrong\n");
        assert_eq!(
            read_ini_key(&gtk, Some("[Settings]"), "gtk-icon-theme-name"),
            None
        );

        // And nothing is not something: an empty value leaves the search where
        // it was rather than naming a theme called "".
        write(&gtk, "[Settings]\ngtk-icon-theme-name=\n");
        assert_eq!(
            read_ini_key(&gtk, Some("[Settings]"), "gtk-icon-theme-name"),
            None
        );
        assert_eq!(
            read_ini_key(&tree.join("absent.ini"), None, "anything"),
            None
        );
    }

    /// Pixels handed over rather than read off the disk: padded rows are
    /// stepped over, three channels means opaque, and a header that does not
    /// describe the buffer behind it is refused.
    ///
    /// The padding is the part worth a test. A sender is free to pad each row
    /// out to a convenient boundary, and reading the block as though it were
    /// tight does not fail — it draws the picture sheared into a diagonal
    /// smear, one row further wrong than the last.
    #[test]
    fn pixels_handed_over_are_read_the_way_they_were_described() {
        // Two red pixels a row, two rows, with four bytes of padding after
        // each. Anything reading it tightly picks the padding up as colour.
        let red = [255u8, 0, 0, 255];
        let padding = [9u8; 4];
        let mut padded = Vec::new();
        for _ in 0..2 {
            padded.extend_from_slice(&red);
            padded.extend_from_slice(&red);
            padded.extend_from_slice(&padding);
        }

        let icon = Icon::from_pixels(2, 2, 12, 4, &padded, 2).expect("a described picture");
        assert_eq!(icon.size, 2);
        assert_eq!(icon.rgba.len(), 2 * 2 * 4);
        assert!(
            icon.rgba.chunks_exact(4).all(|pixel| pixel == red),
            "every pixel is the colour that was sent, not the padding: {:?}",
            icon.rgba
        );

        // Three channels is a picture with nothing transparent in it, and the
        // alpha it does not carry is supplied rather than left at zero — an
        // icon that came out fully transparent would draw as nothing at all.
        let opaque = Icon::from_pixels(1, 1, 3, 3, &[12, 34, 56], 1).expect("three channels");
        assert_eq!(opaque.rgba, vec![12, 34, 56, 255]);

        // A stride shorter than a row, and a buffer shorter than the picture
        // it claims: both describe a walk off the end of what was sent.
        assert!(Icon::from_pixels(2, 2, 4, 4, &padded, 2).is_none());
        assert!(Icon::from_pixels(2, 2, 8, 4, &red, 2).is_none());
        assert!(Icon::from_pixels(0, 2, 8, 4, &padded, 2).is_none());
        assert!(Icon::from_pixels(2, 2, 8, 2, &padded, 2).is_none());
    }

    /// Every one of the shell's own glyphs that has been drawn as a shape
    /// ships as a measurement of that shape rather than as a picture of one,
    /// which is what lets the quad shader cut its own glass to it — see
    /// `glyph_material` in shaders.wgsl.
    ///
    /// Three properties, and the drawing is unusable without all three.
    ///
    /// It has to be *signed*: inside the mark is one side of zero and the air
    /// round it the other, or there is no surface to stand a wall up on. An
    /// opening is air, exactly as the room outside is, and that is the whole
    /// of how a hole gets a ring round it for nothing.
    ///
    /// It has to leave a *margin*. The shader draws the mark's own shadow on
    /// the flat space beside it, and can only draw it where the quad reaches;
    /// a mark running out to the edge of its cell would have its shadow end in
    /// a straight cut.
    ///
    /// And it has to be a *distance*, which is the last assertion and the one
    /// that separates a field from a blurred silhouette: it may not change by
    /// more than a pixel per pixel, anywhere. A chamfer approximation fails
    /// that along the diagonals and a blur fails it everywhere — and either
    /// one produces a bevel that is visibly not a bevel, which is the sort of
    /// thing that gets noticed on screen and nowhere else.
    #[test]
    fn a_glyph_can_ship_as_the_shape_of_itself() {
        let shapes: Vec<&str> = BUILTIN
            .iter()
            .filter(|(_, drawing)| is_shape(drawing))
            .map(|(name, _)| *name)
            .collect();
        assert_eq!(
            shapes.len(),
            BUILTIN.len(),
            "every glyph the shell draws itself is a shape now, and these are \
             not: {:?}",
            BUILTIN
                .iter()
                .filter(|(_, d)| !is_shape(d))
                .map(|(name, _)| *name)
                .collect::<Vec<_>>()
        );

        let size = 128usize;
        for (name, drawing) in BUILTIN.iter().filter(|(_, d)| is_shape(d)) {
            let icon = builtin_distance_field(drawing, size as u32)
                .unwrap_or_else(|| panic!("{name} did not measure"));
            assert_eq!(icon.rgba.len(), size * size * 4);

            // Back out of the encoding, into pixels of the cell.
            let at = |x: usize, y: usize| {
                let stored = f32::from(icon.rgba[(y * size + x) * 4 + 3]) / 255.0;
                (stored - 0.5) * 2.0 * SDF_RANGE * size as f32
            };

            // Signed: some of the cell is mark and some of it is air, and
            // neither is a sliver. A drawing that came out entirely one way is
            // a mask that did not apply or a shape that missed its viewBox.
            let inside = (0..size * size)
                .filter(|i| at(i % size, i / size) < 0.0)
                .count();
            let share = inside as f32 / (size * size) as f32;
            assert!(
                (0.05..0.60).contains(&share),
                "{name} is {share:.3} mark, which is not a mark on a space"
            );

            // The margin the shadow is drawn in: two of the drawing's
            // thirty-two units, which is what the shader's shadow was tuned to
            // reach inside of. Measured as a ring round the cell being air.
            let edge = size / 16;
            for i in 0..size {
                for (x, y) in [
                    (i, edge),
                    (i, size - 1 - edge),
                    (edge, i),
                    (size - 1 - edge, i),
                ] {
                    assert!(
                        at(x.min(size - 1), y.min(size - 1)) > 0.0,
                        "{name} reaches its own edge at {x},{y}"
                    );
                }
            }

            // And it is a distance: one pixel of travel can only ever be one
            // pixel of distance. The stored range saturates far from the edge,
            // which can only make a step smaller, never larger.
            for y in 1..size - 1 {
                for x in 1..size - 1 {
                    let step = (at(x, y) - at(x + 1, y))
                        .abs()
                        .max((at(x, y) - at(x, y + 1)).abs());
                    assert!(step <= 1.35, "{name} steps {step} at {x},{y}");
                }
            }
        }
    }

    /// An application's own icon can be measured as a shape and drawn in the
    /// shell's material, which is what a `.desktop` file asks for by naming
    /// `lxb` — see [`crate::apps::App::wears_shell_material`].
    ///
    /// Two halves and both matter. The measurement has to be a real signed
    /// distance field or the shader reads a picture as one and draws a pale
    /// smear; and [`shaped`] has to answer for the name afterwards, because the
    /// layout never sees the drawing and a slot alone cannot say what it holds.
    ///
    /// A theme's icon that nobody asked about stays a picture. That is the
    /// whole of what makes this opt-in: what the shader is handed is the
    /// silhouette, and a photograph's silhouette is a rounded slab of glass.
    #[test]
    fn an_application_that_asks_gets_its_icon_measured_as_a_shape() {
        let tree = TestTree::new("shaped-app-icon");
        let drawing = concat!(
            "<svg xmlns=\"http://www.w3.org/2000/svg\" viewBox=\"0 0 32 32\">",
            "<circle cx=\"16\" cy=\"16\" r=\"9\" fill=\"#ffffff\"/></svg>"
        );
        write(&tree.join("apps/lxb-test-shaped.svg"), drawing);
        let mut loader = loader_with_dirs(vec![tree.join("apps")]);

        // Nothing has asked yet, so the name is a picture like every other
        // icon out of a theme.
        assert!(!shaped("lxb-test-shaped"));

        let size = 64usize;
        let icon = loader
            .load_shape("lxb-test-shaped", size as u32)
            .expect("the drawing should measure");
        assert_eq!(icon.size, size as u32);

        // Back out of the encoding, the way the built-in glyphs' own test does.
        let at = |x: usize, y: usize| {
            let stored = f32::from(icon.rgba[(y * size + x) * 4 + 3]) / 255.0;
            (stored - 0.5) * 2.0 * SDF_RANGE * size as f32
        };
        assert!(
            at(size / 2, size / 2) < 0.0,
            "the middle is inside the mark"
        );
        assert!(at(1, 1) > 0.0, "and the corner is air");
        for y in 1..size - 1 {
            for x in 1..size - 1 {
                let step = (at(x, y) - at(x + 1, y))
                    .abs()
                    .max((at(x, y) - at(x, y + 1)).abs());
                assert!(step <= 1.35, "it steps {step} at {x},{y}");
            }
        }

        // And once the atlas has said so, every quad carrying the name is given
        // the material.
        remember_shaped_icon("lxb-test-shaped");
        assert!(shaped("lxb-test-shaped"));
        assert!(
            !shaped("lxb-test-not-shaped"),
            "and a theme's icon nobody asked about is still a picture"
        );
    }

    /// A machine with no desktop on it still gets a theme, because hicolor is
    /// not a working default — it is where a program puts its own icon, and it
    /// has never carried the standard names an announcement asks for.
    #[test]
    fn a_session_with_no_desktop_picks_a_theme_that_is_installed() {
        let tree = TestTree::new("themes");
        let root = tree.join("icons");
        let theme = |name: &str, body: &str| {
            std::fs::create_dir_all(root.join(name)).unwrap();
            write(&root.join(name).join("index.theme"), body);
        };

        // Nothing installed but hicolor is nothing to choose.
        theme("hicolor", "[Icon Theme]\nDirectories=48x48/apps\n");
        assert_eq!(any_installed_theme(std::slice::from_ref(&root)), None);

        // A cursor theme is an icon theme by file layout and carries no icons,
        // so picking one would be the same as picking nothing.
        theme("Bibata", "[Icon Theme]\nName=Bibata\n");
        assert_eq!(any_installed_theme(std::slice::from_ref(&root)), None);

        // Anything real will do when there is only one.
        theme("Zafiro", "[Icon Theme]\nDirectories=48x48/apps\n");
        assert_eq!(
            any_installed_theme(std::slice::from_ref(&root)).as_deref(),
            Some("Zafiro")
        );

        // And with a choice, the one most likely to be complete wins over the
        // one that happens to sort first.
        theme("Adwaita", "[Icon Theme]\nDirectories=48x48/apps\n");
        assert_eq!(
            any_installed_theme(std::slice::from_ref(&root)).as_deref(),
            Some("Adwaita")
        );
    }

    #[test]
    fn ranks_scalable_above_fixed_sizes() {
        assert!(directory_rank(Path::new("/t/scalable")) > directory_rank(Path::new("/t/512x512")));
        assert!(directory_rank(Path::new("/t/48x48")) > directory_rank(Path::new("/t/16x16")));
        assert_eq!(directory_rank(Path::new("/t/apps/128")), 128);
        assert_eq!(directory_rank(Path::new("/t/nonsense")), 0);
    }

    #[test]
    fn theme_chain_does_not_loop() {
        let tree = TestTree::new("inheritance-loop");
        write(
            &tree.join("a/index.theme"),
            "[Icon Theme]\nName=A\nInherits=b\n",
        );
        write(
            &tree.join("b/index.theme"),
            "[Icon Theme]\nName=B\nInherits=a\n",
        );

        let mut out = Vec::new();
        let mut seen = HashSet::new();
        collect_theme_chain(std::slice::from_ref(&tree.0), "a", &mut out, &mut seen);
        assert_eq!(out, vec!["a", "b"]);
    }

    #[test]
    fn dotted_desktop_icon_ids_are_not_mistaken_for_extensions() {
        let tree = TestTree::new("dotted-id");
        let directory = tree.join("icons");
        let icon_path = directory.join("org.example.Product.png");
        write(&icon_path, png_bytes(4, 4, [20, 40, 60, 255]));
        let mut loader = loader_with_dirs(vec![directory]);

        assert!(loader.load("org.example.Product", 8).is_some());
        assert_eq!(
            loader.cache.get("org.example.Product"),
            Some(&Some(icon_path))
        );
    }

    #[test]
    fn recognised_suffix_is_removed_without_truncating_other_dots() {
        let tree = TestTree::new("explicit-extension");
        let directory = tree.join("icons");
        let icon_path = directory.join("org.example.Product.png");
        write(&icon_path, png_bytes(4, 4, [20, 40, 60, 255]));
        let mut loader = loader_with_dirs(vec![directory]);

        assert!(loader.load("org.example.Product.png", 8).is_some());
        assert_eq!(
            loader.cache.get("org.example.Product.png"),
            Some(&Some(icon_path))
        );
    }

    #[test]
    fn svg_icons_can_embed_raster_images() {
        let tree = TestTree::new("svg-raster-image");
        let png = tree.join("tile.png");
        let svg = tree.join("icon.svg");
        write(&png, png_bytes(2, 2, [220, 40, 20, 255]));
        write(
            &svg,
            br#"<svg xmlns="http://www.w3.org/2000/svg" width="2" height="2">
                <image href="tile.png" width="2" height="2"/>
            </svg>"#,
        );

        let icon = load_path(&svg, 8).expect("embedded PNG should render");
        assert!(icon
            .rgba
            .chunks_exact(4)
            .any(|pixel| pixel[0] > 150 && pixel[3] > 200));
    }

    #[test]
    fn failed_high_priority_decode_falls_back_and_updates_cache() {
        let tree = TestTree::new("decode-fallback");
        let high = tree.join("high");
        let low = tree.join("low");
        let broken = high.join("sample.svg");
        let fallback = low.join("sample.png");
        write(&broken, "not an SVG");
        write(&fallback, png_bytes(2, 2, [12, 34, 56, 255]));
        let mut loader = loader_with_dirs(vec![high, low]);

        // A stale resolution cache may point at a now-broken file. Loading
        // must still continue into inherited themes when decoding it fails.
        loader.cache.insert("sample".to_string(), Some(broken));
        let icon = loader.load("sample", 2).expect("fallback PNG should load");
        assert_eq!(&icon.rgba[..4], &[12, 34, 56, 255]);
        assert_eq!(loader.cache.get("sample"), Some(&Some(fallback)));
    }

    #[test]
    fn recursively_discovers_nonstandard_theme_layouts() {
        let tree = TestTree::new("nested-layout");
        write(
            &tree.join("Odd/index.theme"),
            "[Icon Theme]\nName=Odd\nDirectories=apps/vector/scalable\n",
        );
        let icon_path = tree.join("Odd/apps/vector/scalable/deep-icon.png");
        write(&icon_path, png_bytes(3, 3, [80, 90, 100, 255]));

        let mut loader =
            IconLoader::from_paths(vec![tree.0.clone()], "Odd".to_string(), Vec::new());
        assert!(loader.load("deep-icon", 8).is_some());
        assert_eq!(loader.cache.get("deep-icon"), Some(&Some(icon_path)));
    }

    #[test]
    fn extensionless_absolute_png_and_svg_are_sniffed() {
        let tree = TestTree::new("extensionless");
        let png = tree.join("RasterIcon");
        let svg = tree.join("VectorIcon");
        write(&png, png_bytes(6, 4, [100, 110, 120, 255]));
        write(
            &svg,
            r##"<svg xmlns="http://www.w3.org/2000/svg" width="4" height="6">
                 <rect width="4" height="6" fill="#ff0000"/>
               </svg>"##,
        );

        for path in [png, svg] {
            let icon = load_path(&path, 8).expect("content sniffing should identify the icon");
            assert_eq!(icon.size, 8);
            assert_eq!(icon.rgba.len(), 8 * 8 * 4);
            assert!(icon.rgba.chunks_exact(4).any(|pixel| pixel[3] != 0));
        }
    }

    #[test]
    fn non_square_rasters_are_centered_without_distortion() {
        let image = image::DynamicImage::ImageRgba8(image::RgbaImage::from_pixel(
            4,
            2,
            image::Rgba([10, 20, 30, 255]),
        ));
        let rgba = fit_raster(image, 8);

        let alpha = |x: usize, y: usize| rgba[(y * 8 + x) * 4 + 3];
        assert_eq!(alpha(4, 0), 0);
        assert_eq!(alpha(4, 2), 255);
        assert_eq!(alpha(4, 5), 255);
        assert_eq!(alpha(4, 7), 0);
    }

    #[test]
    fn svg_pixels_are_converted_back_to_straight_alpha() {
        let mut rgba = [50, 25, 10, 100, 9, 8, 7, 0, 4, 5, 6, 255];
        unpremultiply_rgba(&mut rgba);
        assert_eq!(&rgba[..4], &[128, 64, 26, 100]);
        assert_eq!(&rgba[4..8], &[9, 8, 7, 0]);
        assert_eq!(&rgba[8..], &[4, 5, 6, 255]);
    }

    /// The shell's own glyphs have to be in the binary and have to draw
    /// something, because nothing at runtime will notice if they do not: a
    /// glyph that fails to rasterise leaves an empty space in the sidebar,
    /// which looks like a layout that meant to leave one.
    ///
    /// The whole reason they are built in rather than looked up is that the
    /// quick-settings bars are for a machine with no desktop on it, and so
    /// possibly no icon theme beyond hicolor either.
    #[test]
    fn every_built_in_glyph_ships_and_draws_something() {
        assert_eq!(
            BUILTIN.len(),
            128,
            "a speaker, a struck-out one, a sun, a note, the three transport \
             buttons and the second face of the middle one, a stick pointer, a \
             mixer, a \
             moon, a \
             bell, seven \
             controller buttons, a mouse, four keycaps, four arrows, a \
             keyboard folding away, a power \
             symbol, one per column of the category row — the open carton with \
             an arrow coming down into it that Software wears and the head of \
             the small machine Waydroid runs among them — the two subcategories \
             Multimedia is divided into and the one under Graphics, the two \
             folders that stand for System's Files with the folder, page, drum \
             and house its own rows are drawn with, the \
             forty-one marks the Settings column is drawn from — the brush at \
             the head of its Theme page, and under it the wave for the \
             wallpaper's own material and four of the shell's marks in one \
             cell for the material of the marks, the small window a video \
             floats in with the two sizes it is offered at and the four \
             corners of a screen it can be put in, the two figures the \
             accounts on this machine are reached through with the one figure \
             an account with no picture of its own wears and that figure again \
             with a plus beside it for the account that does not exist yet, \
             and the five an account's own form is made of — a name card, the \
             at sign it logs in by, the key its password is, the framed face of \
             its avatar and the shield \
             saying what it may do — plus its four \
             turns of a monitor, the two links of a chain, hooked and \
             snapped, that a network is joined and left by and the rune \
             everything Bluetooth is reached through wears, the three \
             strengths of the wireless fan the corner of the start screen \
             draws with the six drawings of the battery on the other side of \
             its clock — five levels and the bolt that means filling, \
             the context menu's bin, play mark, ellipsis, sort bars \
             and camera, its two sheets and its sheet with an arrow leaving \
             it with the clipboard the folder they are carried to is chosen \
             under and the pencil a name is changed with, the folder with a \
             cross through it at the head of a listing and the bin with its \
             lid off at the head of the trash, the plus a row that makes one \
             more of something wears, the magnifier at the \
             head of a shelf with the \
             struck-through one that empties it, the padlock on the panel \
             that asks for a password, and the Steam column with the mark every \
             row that came out of it wears, the cycle that asks for the library \
             again and the door its account is left by, and the shell's own \
             fennec at the head of the System information panel"
        );

        for (name, drawing) in BUILTIN {
            assert!(name.starts_with("lxb:"), "{name} could be shadowed");
            assert!(drawing.contains("<svg"), "{name} is not a drawing");

            let icon = Icon::builtin(drawing, 128).unwrap_or_else(|| {
                panic!("{name} did not rasterise");
            });
            assert_eq!(icon.size, 128);
            assert_eq!(icon.rgba.len(), 128 * 128 * 4);

            // Ink, not an empty square. A drawing that misses its viewBox
            // rasterises perfectly happily to nothing at all.
            let ink = icon.rgba.chunks_exact(4).filter(|px| px[3] > 128).count();
            let share = ink as f32 / (128.0 * 128.0);
            assert!(
                (0.05..0.60).contains(&share),
                "{name} covers {share:.3} of its cell"
            );
            // Neutral, so that the colour the layout asks for is the colour it
            // gets: the atlas multiplies the quad's colour into the texel, and
            // a glyph with a hue of its own could only ever come out muddier
            // than the label beside it.
            //
            // Not the same as *white*. Everything the guide draws is shaded —
            // one lamp above the drawing, a shadow under it — and a grey under
            // a white tint is still that grey. What must not vary is the
            // balance between the channels.
            for pixel in icon.rgba.chunks_exact(4).filter(|px| px[3] == 255) {
                let [r, g, b] = [pixel[0], pixel[1], pixel[2]];
                assert!(
                    r.abs_diff(g) <= 1 && g.abs_diff(b) <= 1 && r.abs_diff(b) <= 1,
                    "{name} has a colour of its own: {:?}",
                    &pixel[..3]
                );
                // And never so dark that it reads as a hole in the glyph.
                assert!(r >= 128, "{name} has a pixel at {r}, which is not ink");
            }
        }

        // Distinct drawings, every one of them: muting has to be visible as
        // more than a change of alpha, a hint that named the same button twice
        // would be worse than no hint, and four arrow caps that rasterised
        // alike would point the wrong way three times out of four.
        let names: Vec<&str> = BUILTIN.iter().map(|(name, _)| *name).collect();
        assert_eq!(
            names,
            vec![
                VOLUME,
                VOLUME_MUTED,
                BRIGHTNESS,
                MEDIA_VOLUME,
                MEDIA_PREVIOUS,
                MEDIA_PLAY,
                MEDIA_PAUSE,
                MEDIA_NEXT,
                POINTER_STICK,
                VOLUME_MIXER,
                DO_NOT_DISTURB,
                NOTIFICATIONS,
                PAD_SELECT,
                PAD_WEST,
                PAD_SOUTH,
                PAD_EAST,
                PAD_START,
                PAD_NORTH,
                PAD_GUIDE,
                MOUSE_RIGHT,
                KEY_ESCAPE,
                KEY_SPACE,
                KEY_ENTER,
                KEY_SUPER,
                ARROW_LEFT,
                ARROW_DOWN,
                ARROW_UP,
                ARROW_RIGHT,
                KEYBOARD_HIDE,
                SHUTDOWN,
                CATEGORY_SETTINGS,
                CATEGORY_SYSTEM,
                CATEGORY_MULTIMEDIA,
                CATEGORY_GRAPHICS,
                CATEGORY_INTERNET,
                CATEGORY_OFFICE,
                CATEGORY_GAMES,
                CATEGORY_SOFTWARE,
                CATEGORY_DEVELOPMENT,
                CATEGORY_EDUCATION,
                CATEGORY_UTILITIES,
                CATEGORY_WAYDROID,
                CATEGORY_OTHER,
                CATEGORY_MUSIC,
                CATEGORY_VIDEO,
                CATEGORY_IMAGES,
                CATEGORY_FILES,
                FILE_FOLDER,
                FILE_PAGE,
                FILE_DRIVE,
                FILE_HOME,
                SETTING_APPEARANCE,
                SETTING_ACCENT,
                SETTING_THEME,
                SETTING_WALLPAPER,
                SETTING_ICONS,
                SETTING_DISPLAY,
                SETTING_RESOLUTION,
                SETTING_REFRESH,
                SETTING_ORIENTATION,
                SETTING_ROTATION_0,
                SETTING_ROTATION_90,
                SETTING_ROTATION_180,
                SETTING_ROTATION_270,
                SETTING_ORDER,
                SETTING_NIGHT_LIGHT,
                SETTING_SCHEDULE,
                SETTING_HDR,
                SETTING_SCREEN_REST,
                SETTING_MICROPHONE,
                SETTING_NETWORK,
                SETTING_ADDRESS,
                SETTING_NAME_SERVER,
                SETTING_TYPED,
                SETTING_CONNECT,
                SETTING_DISCONNECT,
                SETTING_WIFI,
                SETTING_ETHERNET,
                SIGNAL_WEAK,
                SIGNAL_FAIR,
                SIGNAL_STRONG,
                BATTERY_EMPTY,
                BATTERY_LOW,
                BATTERY_HALF,
                BATTERY_HIGH,
                BATTERY_FULL,
                BATTERY_CHARGING,
                SETTING_BLUETOOTH,
                SETTING_SYSTEM,
                SETTING_SCALE,
                SETTING_USERS,
                SETTING_PERSON,
                SETTING_ADD_USER,
                SETTING_NAME,
                SETTING_USERNAME,
                SETTING_PASSWORD,
                SETTING_AVATAR,
                SETTING_ACCOUNT_TYPE,
                SETTING_PIP,
                SETTING_PIP_SIZE,
                SETTING_PIP_TOP_LEFT,
                SETTING_PIP_TOP_RIGHT,
                SETTING_PIP_BOTTOM_LEFT,
                SETTING_PIP_BOTTOM_RIGHT,
                SETTING_INFO,
                SWATCH,
                CHOSEN,
                ADD,
                UNINSTALL,
                LAUNCH,
                OPEN_WITH,
                SORT,
                COPY,
                MOVE,
                PASTE,
                RENAME,
                NEW_FOLDER,
                TRASH_EMPTY,
                SELECT_MULTIPLE,
                SCREENSHOT,
                SEARCH,
                SEARCH_CLEAR,
                AUTHENTICATE,
                CATEGORY_STEAM,
                STEAM,
                REFRESH,
                SIGN_OUT,
                LOGO
            ]
        );
        let drawn: Vec<Vec<u8>> = BUILTIN
            .iter()
            .map(|(_, drawing)| Icon::builtin(drawing, 64).unwrap().rgba)
            .collect();
        for (first, left) in drawn.iter().enumerate() {
            for (second, right) in drawn.iter().enumerate().skip(first + 1) {
                assert_ne!(
                    left, right,
                    "{} and {} rasterise the same",
                    names[first], names[second]
                );
            }
        }
    }
}
