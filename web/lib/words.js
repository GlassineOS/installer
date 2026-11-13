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

// Boot settings (vocab BOOT_SETTING_KINDS).
export const BOOT_SETTING = {
  'boot-loader': 'The boot loader, the program that starts the device',
  'slot-flags': 'Which start-up option starts',
  'firmware-boot-entry': 'The firmware’s start-up entries',
  'firmware-boot-order': 'The order of the start-up entries',
};

// Device data the installation keeps (descriptions' device_data).
export const DEVICE_DATA = {
  'gpu-firmware': 'graphics firmware',
  'sensor-calibration': 'sensor calibration',
  'modem-firmware': 'modem firmware',
  'wifi-firmware': 'Wi-Fi firmware',
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

// The partition an artifact role goes to, for a least size the release states
// (plan space.minimums[].role; vocab ARTIFACT_ROLES).
export const PARTITION_FOR = {
  base: 'GlassineOS’s system partition',
  kernel: 'GlassineOS’s boot partition',
  recovery: 'the recovery partition',
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
