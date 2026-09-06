#include <stdbool.h>
#include <stdint.h>
#import <AppKit/AppKit.h>

BOOL fixtureSetClipboardBytes(NSPasteboardItem *item, const void *bytes,
                             NSUInteger length, NSString *type);

// 0 = success, 1 = secure field, 2 = insertion error, 3 = panic/wrong thread.
int32_t ptt_test_begin(const char *text, bool append_space, void **output);
int32_t ptt_test_paste(void *handle);
int32_t ptt_test_finish(void *handle);
uint64_t ptt_test_settle_ms(void);
uint64_t ptt_test_restore_ms(void);
