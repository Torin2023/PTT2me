#import <AppKit/AppKit.h>

// Keep the in-process pasteboard cache in Foundation's NSData class cluster.
// Swift Data otherwise supplies __NSSwiftData, whose `length` encoding differs
// from NSData on some macOS versions and trips objc2's debug ABI validation.
// No representation or bytes are changed by this fixture-only adapter.
BOOL fixtureSetClipboardBytes(NSPasteboardItem *item, const void *bytes,
                             NSUInteger length, NSString *type) {
    return [item setData:[NSData dataWithBytes:bytes length:length] forType:type];
}
