// A window that changes 60 times a second, for the CI's window-capture test (osascript -l JavaScript).
ObjC.import("Cocoa");
const app = $.NSApplication.sharedApplication;
app.setActivationPolicy($.NSApplicationActivationPolicyRegular);
const w = $.NSWindow.alloc.initWithContentRectStyleMaskBackingDefer($.NSMakeRect(100, 100, 1280, 720), 15, $.NSBackingStoreBuffered, false);
w.title = "VIT capture test";
const label = $.NSTextField.alloc.initWithFrame($.NSMakeRect(80, 300, 1100, 120));
label.font = $.NSFont.boldSystemFontOfSize(72);
label.bezeled = false; label.editable = false; label.drawsBackground = false;
w.contentView.addSubview(label);
w.makeKeyAndOrderFront(null);
app.activateIgnoringOtherApps(true);
for (let i = 0; i < 60 * 40; i++) {
  w.backgroundColor = $.NSColor.colorWithCalibratedRedGreenBlueAlpha((i % 60) / 60, 0.3, 1 - (i % 60) / 60, 1);
  label.stringValue = "Frame " + i;
  w.display;
  $.NSRunLoop.currentRunLoop.runUntilDate($.NSDate.dateWithTimeIntervalSinceNow(1 / 60));
}
