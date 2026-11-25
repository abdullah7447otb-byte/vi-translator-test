// A window captured with ScreenCaptureKit (macOS 12.3 and later): the system sends each new
// picture of the window as it's drawn, from the GPU, instead of the app copying the whole window
// out of the window server every time it asks (CGWindowListCreateImage, deprecated). The window is
// captured on its own, so what covers it (the app's overlay too) isn't in the picture.

#import <Foundation/Foundation.h>
#import <AppKit/AppKit.h>
#import <CoreMedia/CoreMedia.h>
#import <CoreVideo/CoreVideo.h>
#import <ScreenCaptureKit/ScreenCaptureKit.h>
#include <stdint.h>
#include <stdlib.h>
#include <string.h>
#include <os/lock.h>

API_AVAILABLE(macos(12.3))
@interface VitSck : NSObject <SCStreamOutput, SCStreamDelegate>
@property (strong) SCStream *stream;
@property (strong) dispatch_queue_t queue;
@end

@implementation VitSck {
@public
	os_unfair_lock lock;
	uint8_t *bgra;
	size_t cap, stride;
	uint32_t w, h;
	uint64_t seq;
	int failed;
}

- (void)stream:(SCStream *)stream didOutputSampleBuffer:(CMSampleBufferRef)sb ofType:(SCStreamOutputType)type {
	if (type != SCStreamOutputTypeScreen || !CMSampleBufferIsValid(sb)) return;
	// (frames with nothing new in them come too: only "complete" ones carry a picture)
	CFArrayRef att = CMSampleBufferGetSampleAttachmentsArray(sb, false);
	if (att && CFArrayGetCount(att) > 0) {
		CFDictionaryRef d = CFArrayGetValueAtIndex(att, 0);
		CFNumberRef st = CFDictionaryGetValue(d, (__bridge CFStringRef)SCStreamFrameInfoStatus);
		int status = 0;
		if (st && CFNumberGetValue(st, kCFNumberIntType, &status) && status != SCFrameStatusComplete) return;
	}
	CVPixelBufferRef px = CMSampleBufferGetImageBuffer(sb);
	if (!px) return;
	if (CVPixelBufferLockBaseAddress(px, kCVPixelBufferLock_ReadOnly) != kCVReturnSuccess) return;
	size_t pw = CVPixelBufferGetWidth(px), ph = CVPixelBufferGetHeight(px), row = CVPixelBufferGetBytesPerRow(px);
	const uint8_t *base = CVPixelBufferGetBaseAddress(px);
	os_unfair_lock_lock(&lock);
	if (base && row * ph > cap) {
		uint8_t *n = realloc(bgra, row * ph);
		if (n) { bgra = n; cap = row * ph; }
	}
	if (base && row * ph <= cap) {
		memcpy(bgra, base, row * ph);
		stride = row;
		w = (uint32_t)pw;
		h = (uint32_t)ph;
		seq++;
	}
	os_unfair_lock_unlock(&lock);
	CVPixelBufferUnlockBaseAddress(px, kCVPixelBufferLock_ReadOnly);
}

- (void)stream:(SCStream *)stream didStopWithError:(NSError *)error {
	os_unfair_lock_lock(&lock);
	failed = 1;
	os_unfair_lock_unlock(&lock);
}
@end

static void put_err(char *err, size_t len, NSString *s) {
	if (err && len) snprintf(err, len, "%s", s.UTF8String ? s.UTF8String : "ScreenCaptureKit failed");
}

static SCStreamConfiguration *config_for(double pw, double ph, int nominal, int fps) API_AVAILABLE(macos(12.3)) {
	SCStreamConfiguration *c = [SCStreamConfiguration new];
	double scale = nominal ? 1.0 : (NSScreen.mainScreen ? NSScreen.mainScreen.backingScaleFactor : 2.0);
	c.width = (size_t)(pw * scale);
	c.height = (size_t)(ph * scale);
	c.pixelFormat = kCVPixelFormatType_32BGRA;
	c.minimumFrameInterval = CMTimeMake(1, fps > 0 ? fps : 60);
	c.queueDepth = 3;
	c.showsCursor = NO;
	if (@available(macOS 13.0, *)) c.capturesAudio = NO;
	return c;
}

// Starts capturing window `window_id` (points wide × high; `nominal`: one pixel a point).
// NULL and `err` filled if ScreenCaptureKit can't (too old a macOS, no permission, no window).
void *vit_sck_start(uint32_t window_id, double pw, double ph, int nominal, int fps, char *err, size_t errlen) {
	if (@available(macOS 12.3, *)) {
		__block SCShareableContent *content = nil;
		__block NSError *e = nil;
		dispatch_semaphore_t done = dispatch_semaphore_create(0);
		[SCShareableContent getShareableContentExcludingDesktopWindows:YES onScreenWindowsOnly:NO
			completionHandler:^(SCShareableContent *c, NSError *error) {
				content = c;
				e = error;
				dispatch_semaphore_signal(done);
			}];
		if (dispatch_semaphore_wait(done, dispatch_time(DISPATCH_TIME_NOW, 5 * NSEC_PER_SEC)) != 0) {
			put_err(err, errlen, @"ScreenCaptureKit didn't answer");
			return NULL;
		}
		if (!content) {
			put_err(err, errlen, e ? e.localizedDescription : @"no shareable content");
			return NULL;
		}
		SCWindow *win = nil;
		for (SCWindow *x in content.windows) if (x.windowID == window_id) { win = x; break; }
		if (!win) {
			put_err(err, errlen, @"the window isn't shareable");
			return NULL;
		}
		SCContentFilter *filter = [[SCContentFilter alloc] initWithDesktopIndependentWindow:win];
		VitSck *s = [VitSck new];
		s->lock = OS_UNFAIR_LOCK_INIT;
		s.queue = dispatch_queue_create("vit.sck", DISPATCH_QUEUE_SERIAL);
		s.stream = [[SCStream alloc] initWithFilter:filter configuration:config_for(pw, ph, nominal, fps) delegate:s];
		NSError *ae = nil;
		if (![s.stream addStreamOutput:s type:SCStreamOutputTypeScreen sampleHandlerQueue:s.queue error:&ae]) {
			put_err(err, errlen, ae.localizedDescription);
			return NULL;
		}
		__block NSError *se = nil;
		dispatch_semaphore_t started = dispatch_semaphore_create(0);
		[s.stream startCaptureWithCompletionHandler:^(NSError *error) {
			se = error;
			dispatch_semaphore_signal(started);
		}];
		if (dispatch_semaphore_wait(started, dispatch_time(DISPATCH_TIME_NOW, 5 * NSEC_PER_SEC)) != 0 || se) {
			put_err(err, errlen, se ? se.localizedDescription : @"the capture didn't start");
			return NULL;
		}
		return (__bridge_retained void *)s;
	}
	put_err(err, errlen, @"ScreenCaptureKit needs macOS 12.3 or later");
	return NULL;
}

// The newest picture if it's newer than `have` (RGBA into `dst`, packed): 1 copied, 0 nothing
// new, -1 the stream stopped, -2 `dst` too small (*w and *h say what's needed).
int vit_sck_frame(void *p, uint64_t have, uint8_t *dst, size_t dst_len, uint32_t *w, uint32_t *h, uint64_t *seq) {
	if (@available(macOS 12.3, *)) {
		VitSck *s = (__bridge VitSck *)p;
		os_unfair_lock_lock(&s->lock);
		int r = 0;
		if (s->failed) r = -1;
		else if (s->seq != have && s->seq > 0) {
			*w = s->w; *h = s->h; *seq = s->seq;
			if ((size_t)s->w * s->h * 4 > dst_len) r = -2;
			else {
				for (uint32_t y = 0; y < s->h; y++) {
					const uint8_t *src = s->bgra + (size_t)y * s->stride;
					uint8_t *out = dst + (size_t)y * s->w * 4;
					for (uint32_t x = 0; x < s->w; x++, src += 4, out += 4) {
						out[0] = src[2]; out[1] = src[1]; out[2] = src[0]; out[3] = 255;
					}
				}
				r = 1;
			}
		}
		os_unfair_lock_unlock(&s->lock);
		return r;
	}
	return -1;
}

// The window changed size: the pictures follow it.
void vit_sck_resize(void *p, double pw, double ph, int nominal, int fps) {
	if (@available(macOS 12.3, *)) {
		VitSck *s = (__bridge VitSck *)p;
		[s.stream updateConfiguration:config_for(pw, ph, nominal, fps) completionHandler:^(NSError *e) { (void)e; }];
	}
}

void vit_sck_stop(void *p) {
	if (!p) return;
	if (@available(macOS 12.3, *)) {
		VitSck *s = (__bridge_transfer VitSck *)p;
		dispatch_semaphore_t done = dispatch_semaphore_create(0);
		[s.stream stopCaptureWithCompletionHandler:^(NSError *e) { (void)e; dispatch_semaphore_signal(done); }];
		dispatch_semaphore_wait(done, dispatch_time(DISPATCH_TIME_NOW, 2 * NSEC_PER_SEC));
		os_unfair_lock_lock(&s->lock);
		free(s->bgra);
		s->bgra = NULL;
		s->cap = 0;
		os_unfair_lock_unlock(&s->lock);
	}
}
