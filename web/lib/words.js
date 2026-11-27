// SPDX-License-Identifier: GPL-2.0-or-later
// The screens' words for the core's vocabulary (core/src/vocab.rs). The records carry ids;
// what a person reads comes from here, or from the device description's texts. These are the
// same for every machine (spec 001 SC-005): nothing here names a device or a system.

export const FLOW = [
  { id: 'check', name: 'Check' },
  { id: 'plan', name: 'Plan' },
  { id: 'confirm', name: 'Confirm' },
  { id: 'backup', name: 'Back up' },
  { id: 'install', name: 'Install' },
  { id: 'verify', name: 'Verify' },
];

export const BROWSERS = 'Chromium, Google Chrome, Microsoft Edge, Brave, Opera or Vivaldi';

// Capabilities (vocab CAPABILITIES). status: have | lack | needs.
export const CAPABILITY = {
  'start-kernel': { name: 'Choose which kernel starts', body: 'GlassineOS can start the kernel of its choice.' },
  'fallback-boot': { name: 'Fall back to the previous start', body: 'When a new start isn’t accepted, the previous one starts again.' },
  'trial-boot': { name: 'Trial start', body: 'A new kernel or version starts once, on trial.' },
  recovery: { name: 'Recovery', body: 'GlassineOS’s own repair tool, started on the machine itself.' },
  'keep-stock': { name: 'Keep the maker’s system', body: 'The system it came with stays next to GlassineOS.' },
  'secondary-recovery': { name: 'The kept system’s own recovery', body: 'The recovery of the system it came with stays reachable.' },
  'choose-at-start': { name: 'Pick a system at start-up', body: 'You choose which system starts.' },
};
export const CAPABILITY_STATUS = { have: 'Yes', lack: 'No', needs: 'Not yet' };

// Install states (vocab STANDARD_STATES).
export const STATE = {
  stock: { name: 'As it came from the maker', body: 'No part of GlassineOS is on it.', offer: 'The installer offers to install.' },
  prepared: { name: 'Prepared', body: 'Parts of GlassineOS’s start-up are in place, no GlassineOS yet.', offer: 'The installer continues from there.' },
  installed: { name: 'GlassineOS installed', body: 'GlassineOS is installed on it.', offer: 'The installer offers repair, reinstall, or back to stock.' },
};

// Where a step leaves the machine (vocab ESCAPE_LEAVES); `the` is "the <machine>".
export const LEAVES = {
  'previous-system-starts': (the) => `Nothing is lost: ${the} starts its previous system as before.`,
  'glassine-starts': () => 'GlassineOS starts.',
  'installer-can-finish': () => 'The installer can finish the installation from here.',
  'installer-can-undo': () => 'The installer can undo it, putting the backup back.',
  'special-hardware': (the, hw) => `${cap(the)} can then be reached only with ${hw ? 'the ' + hardwareName(hw) : 'special hardware'}.`,
};

// The three ends of a cut-off installation (spec 001 Scenario 4), by `leaves`.
export const ENDS = [
  { leaves: ['previous-system-starts'], name: 'Its previous system starts', body: 'After an undo, or when the cut came before anything was replaced.' },
  { leaves: ['glassine-starts'], name: 'GlassineOS starts', body: 'After the installation is resumed and finished.' },
  { leaves: ['installer-can-finish', 'installer-can-undo'], name: 'The installer can finish or undo it', body: 'Resume it, or undo it.' },
];

// Special hardware a plan can ask about (vocab NEEDS, SPECIAL_HARDWARE).
export const HARDWARE = {
  'emergency-cable': { name: 'emergency cable', what: 'A USB cable that starts the device in its emergency mode, where it can always be written.' },
};
export const hardwareName = (id) => (HARDWARE[id] ? HARDWARE[id].name : id);

// A file the person supplies, by the route of the mode that needs it (spec 004; the mode's kind,
// vocab MODE_KINDS): what it is called on every device of that route.
export const SUPPLIED_ROUTE = {
  'emergency-download': { noun: 'emergency programmer', short: 'programmer' },
};
export const suppliedWords = (route) => SUPPLIED_ROUTE[route] || { noun: 'file', short: 'file' };

// The values a supplied file must match (vocab SUPPLIED_MATCHES keys).
export const SUPPLIED_MATCH = { chip: 'Chip', pk_hash: 'Signing key' };

// Boot settings (vocab BOOT_SETTING_KINDS).
export const BOOT_SETTING = {
  'boot-loader': 'The boot loader, the program that starts the device',
  'slot-flags': 'Which start-up option starts',
  'firmware-boot-entry': 'The firmware’s start-up entries',
  'firmware-boot-order': 'The order of the start-up entries',
};

// How the installer reaches the machine (report runs_on.connections; mode kinds).
export const CONNECTION = {
  'usb-native': { name: 'USB, from this computer', note: 'The installer talks to the device over USB.' },
  webusb: { name: 'USB, through this browser', note: 'Only the device you choose in the browser’s window, and only from this page.' },
  local: { name: 'This PC’s own disks and firmware', note: 'The installer runs on this PC.' },
};
export const MODE = {
  fastboot: 'Its boot loader’s USB mode',
  'fastboot-stock': 'Its boot loader’s USB mode',
  'android-adb': 'Android’s USB debugging',
  'emergency-download': 'Its emergency mode',
  'glassine-ram': 'A GlassineOS environment started from memory',
  'glassine-usb': 'GlassineOS’s own USB mode',
  local: 'The installer, running on this PC',
  installer: 'The installer',
};

// When the browser couldn't open or claim a device's USB mode (the core's error kinds permission
// and busy; lib/host.js): why, and the host setup that is the way out (core/src/host_setup.rs).
// busyHeld: Linux, for a mode a kernel driver takes as soon as it appears (its record entry
// names the driver the setup lets go of).
export const HOST_ACCESS = {
  cause: {
    permission: 'This computer didn’t let the browser open the device.',
    busy: 'Something on this computer holds the device, so the browser couldn’t take it.',
  },
  linux: {
    permission: 'On Linux only root may open a device in this mode until the computer is set up for it.',
    busy: 'Close any other program or browser tab that uses the device, unplug it, plug it in again, and choose it again.',
    busyHeld: 'In this mode that is most likely the system’s own serial driver: it takes the device as soon as it is connected, and a browser can’t take it back (another program or browser tab using it does the same: close it).',
    setup: (held) => `Set this computer up once: open a terminal, paste this line and press Enter. It asks for your password. Afterwards you can open the device in each of its USB modes without being root${held ? ', the modem manager leaves it alone, and the serial driver lets go of it' : ' and the modem manager leaves it alone'}.`,
    label: 'One line for a terminal',
    shows: (path) => `What it writes: ${path}`,
    after: 'Then unplug the device, plug it in again, and choose it again.',
  },
  windows: {
    setup: (ids) => `On Windows the device needs the WinUSB driver in this mode, installed once with Zadig: in its Options menu choose List All Devices, pick the device whose USB ID is ${list(ids)}, choose WinUSB and click Install Driver (or Replace Driver).`,
    after: 'Then unplug the device, plug it in again, and choose it again.',
  },
  other: 'Close any other program or browser tab that uses the device, unplug it, plug it in again, and choose it again.',
  copy: 'Copy',
  copied: 'Copied',
  selected: 'Selected: copy it',
};

// The partition an artifact role goes to, for a least size the release states
// (plan space.minimums[].role; vocab ARTIFACT_ROLES).
export const PARTITION_FOR = {
  base: 'GlassineOS’s system partition',
  kernel: 'GlassineOS’s boot partition',
  recovery: 'the recovery partition',
};

// What an artifact is, for a partition that keeps it at the end of a plan that leaves a
// step out (plan left_out[].holds_instead[].artifact; vocab ARTIFACT_ROLES).
export const ARTIFACT = {
  'ram-environment': 'the installer’s helper',
  recovery: 'GlassineOS’s recovery',
  kernel: 'GlassineOS’s kernel',
  base: 'GlassineOS',
  'boot-loader': 'GlassineOS’s boot loader',
  'partition-layout': 'the partition table',
};

// Operating systems found (report systems[].kind; plan systems[].system is "system N (kind)").
export const SYSTEM_KIND = { windows: 'Windows', linux: 'Linux', macos: 'macOS', android: 'Android', other: 'Another system' };

// The jobs on an installed machine (vocab JOBS).
export const JOB = {
  repair: { title: 'Repair', body: 'Start-up and recovery are put back. GlassineOS’s versions and your storage are kept.' },
  reinstall: { title: 'Reinstall', body: 'A fresh first version of GlassineOS. Your storage is kept.' },
  'back-to-stock': { title: 'Back to stock', body: 'The backup goes back: the machine is as it was before the installation.' },
};

export function cap(s) { return s ? s.charAt(0).toUpperCase() + s.slice(1) : s; }

// "a, b and c"
export function list(xs) {
  if (xs.length <= 1) return xs.join('');
  return xs.slice(0, -1).join(', ') + ' and ' + xs[xs.length - 1];
}

// The core's size words (core/src/plan.rs human()).
export function human(n) {
  if (n === null || n === undefined) return null;
  const U = ['B', 'KiB', 'MiB', 'GiB', 'TiB'];
  let x = n, u = 0;
  while (x >= 1024 && u < U.length - 1) { x /= 1024; u++; }
  return u === 0 ? `${n} B` : `${x.toFixed(1)} ${U[u]}`;
}

// Making the PC installer drive in the browser (spec 005). A browser can't write a USB stick:
// these words never say or suggest that the page writes it.
export const DRIVE = {
  eyebrow: 'A PC',
  title: 'Make a USB stick for a PC',
  lead: 'GlassineOS is installed on a PC from a USB stick started on that PC. Here you download the stick’s file and check it. An imaging program then writes it to the stick: a browser can’t write to a USB stick.',
  need: 'You need',
  needStick: (gb) => (gb ? `A USB stick of at least ${gb} GB. Everything on it is erased.` : 'A USB stick big enough for the unpacked file. Everything on it is erased.'),
  needPc: 'A PC with a 64-bit Intel or AMD processor that starts with UEFI, as most PCs made since 2012 do.',
  needSecureBoot: 'Secure Boot switched off on that PC. Below it says how.',
  looking: 'Looking for the newest stick…',
  offline: { title: 'The list of releases couldn’t be read', body: 'This computer got no answer. Check its connection to the internet, then try again.' },
  refused: { title: 'The list of releases couldn’t be read', body: 'GitHub didn’t give the list just now. It gives a computer only a few lists an hour; try again later.' },
  none: { title: 'No stick is published yet', body: 'No GlassineOS release for PCs carries a stick’s file yet.' },
  listed: 'The releases are listed here:',
  again: 'Try again',
  steps: ['Download the file', 'Check the file', 'Write it to the USB stick', 'Start the PC from the stick'],
  release: (base) => `GlassineOS for PCs, ${base}`,
  published: (date) => `Published ${date}`,
  unreviewed: 'GlassineOS is unreviewed and has been tested only on virtual PCs. Installing it on a real PC is at your own risk.',
  download: (size) => `Download (${size})`,
  downloadNote: 'Your browser saves the file, usually in your Downloads folder.',
  checkBody: 'When the download is finished, choose the file here. The installer checks on this computer that it is the published file, byte for byte. Nothing is uploaded.',
  choose: 'Choose the downloaded file…',
  chooseAgain: 'Choose another file…',
  checking: (pct) => `Checking the file… ${pct} %`,
  result: {
    match: { name: 'The published file', body: 'It is the published file, byte for byte. Write it to the USB stick.' },
    short: { name: 'Not the whole file', body: (got, want) => `It has ${got}, the published file ${want}. Wait until the download has finished, or download it again.` },
    long: { name: 'Not the published file', body: (got, want) => `It has ${got}, the published file ${want}. Choose the file you downloaded here.` },
    differs: { name: 'Not the published file', body: 'Its checksum differs from the published one: it was changed, or the download went wrong.' },
    unreadable: { name: 'The file couldn’t be read', body: 'Choose it again, or download it again.' },
  },
  checksum: 'Checksum',
  publishedSum: 'Published',
  dontWrite: { label: 'Don’t write it', title: 'Don’t write this file to a stick', body: 'Download the file again and check it. Nothing has been written anywhere.' },
  checkFirst: 'Check the file first. The steps appear here when it is the published file.',
  writeIntro: 'An imaging program writes the file to the stick and erases everything that was on it. Use one of these. They are free.',
  system: 'This computer runs',
  systems: { windows: 'Windows', macos: 'macOS', linux: 'Linux' },
  imagers: [
    { id: 'etcher', name: 'balenaEtcher', steps: ['Choose Flash from file and pick the checked file.', 'Choose Select target and pick the USB stick.', 'Choose Flash. It checks the stick when it has written it.'] },
    { id: 'fmw', name: 'Fedora Media Writer', steps: ['Choose Select .iso file and pick the checked file. If it isn’t listed, show all files in the file window.', 'Pick the USB stick, then choose Write.'] },
    { id: 'rpi', name: 'Raspberry Pi Imager', steps: ['For the operating system choose Use custom and pick the checked file.', 'For the storage pick the USB stick.', 'When it offers to customise the system, say no: that is for Raspberry Pi OS. Then write.'] },
  ],
  dd: {
    name: 'dd, in a terminal',
    find: 'Plug in the stick and find its name. Its line shows usb, and its size:',
    write: 'Then write the file. Put the stick’s name in place of sdX, and run the line in the folder with the file. A wrong name erases another disk.',
    unpack: { xz: 'xz -dc', zst: 'zstd -dc', gz: 'gzip -dc' },
  },
  copy: 'Copy',
  copied: 'Copied',
  secureBoot: {
    label: 'Secure Boot',
    title: 'The PC starts the stick only with Secure Boot off',
    body: 'GlassineOS’s start program isn’t signed for Secure Boot. With Secure Boot on, the PC doesn’t start the stick: it shows a message or starts its usual system, and nothing on it changes.',
    how: 'To switch it off, open the PC’s firmware set-up: press its set-up key while it starts, often F2, Del or Esc. Find Secure Boot, usually under Boot or Security, set it to Disabled, save and leave.',
  },
  start: [
    'Plug the stick into the PC and switch the PC on.',
    'Press its boot menu key while it starts, often F12, F11, F10, F8 or Esc, and choose the USB stick. If it is listed twice, choose the one with UEFI in its name.',
    'GlassineOS starts from the stick. The installation goes on there: it asks what it needs, and writes nothing to the PC until you confirm.',
  ],
  next: { before: 'Download the file, then choose it here for the check.', match: 'Write the file with an imaging program, then start the PC from the stick.', bad: 'Download the file again and check it.', error: 'Try again later. Nothing has happened.', none: 'Nothing, until a stick is published.' },
  out: 'Close this page. Nothing has been written anywhere.',
  back: 'Back to the start',
};
