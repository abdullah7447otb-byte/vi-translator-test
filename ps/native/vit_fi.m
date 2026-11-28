// Motion smoothing (Barqawi, macOS): a frame made between two of the PS5's, by VideoToolbox's low-latency
// frame interpolation (VTLowLatencyFrameInterpolation, macOS 26). GTA VI draws 30 frames a second and Remote
// Play sends 60, so every other frame is the one before it again; vit_vt.c puts a frame made halfway between
// its neighbours in its place.
//
// The classes are looked up when the app runs (NSClassFromString and their selectors), not linked: the app is
// built with whatever Xcode the build machine has, and runs on Macs before macOS 26, where this is just off.

#ifdef __APPLE__

#import <Foundation/Foundation.h>
#import <CoreMedia/CoreMedia.h>
#import <CoreVideo/CoreVideo.h>
#import <VideoToolbox/VideoToolbox.h>
#include <objc/message.h>

typedef struct VitFi {
	void *cfg;    // (__bridge_retained) VTLowLatencyFrameInterpolationConfiguration
	void *proc;   // (__bridge_retained) VTFrameProcessor
	CVPixelBufferPoolRef src_pool, dst_pool, out_pool;
	VTPixelTransferSessionRef xfer;
	int w, h;
	int64_t pts;
} VitFi;

static Class cls(NSString *name) { return NSClassFromString(name); }

// Whether this Mac has the processor (macOS 26 and the hardware for it).
int vit_fi_supported(void) {
	@autoreleasepool {
		Class C = cls(@"VTLowLatencyFrameInterpolationConfiguration");
		SEL s = NSSelectorFromString(@"isSupported");
		if (!C || !cls(@"VTFrameProcessor") || !cls(@"VTFrameProcessorFrame") || !cls(@"VTLowLatencyFrameInterpolationParameters")) return 0;
		if (![C respondsToSelector:s]) return 0;
		return ((BOOL (*)(id, SEL))objc_msgSend)(C, s) ? 1 : 0;
	}
}

static void drop_all(VitFi *fi) {
	if (fi->proc) {
		id proc = (__bridge_transfer id)fi->proc;
		SEL end = NSSelectorFromString(@"endSession");
		if ([proc respondsToSelector:end]) ((void (*)(id, SEL))objc_msgSend)(proc, end);
		proc = nil;
		fi->proc = NULL;
	}
	if (fi->cfg) {
		id c = (__bridge_transfer id)fi->cfg;
		c = nil;
		fi->cfg = NULL;
	}
	if (fi->src_pool) { CVPixelBufferPoolRelease(fi->src_pool); fi->src_pool = NULL; }
	if (fi->dst_pool) { CVPixelBufferPoolRelease(fi->dst_pool); fi->dst_pool = NULL; }
	if (fi->out_pool) { CVPixelBufferPoolRelease(fi->out_pool); fi->out_pool = NULL; }
	fi->w = fi->h = 0;
}

VitFi *vit_fi_new(void) {
	if (!vit_fi_supported()) return NULL;
	VitFi *fi = calloc(1, sizeof(VitFi));
	if (!fi) return NULL;
	if (VTPixelTransferSessionCreate(NULL, &fi->xfer) != noErr) { free(fi); return NULL; }
	return fi;
}

void vit_fi_free(VitFi *fi) {
	if (!fi) return;
	@autoreleasepool { drop_all(fi); }
	if (fi->xfer) { VTPixelTransferSessionInvalidate(fi->xfer); CFRelease(fi->xfer); }
	free(fi);
}

static CVPixelBufferPoolRef pool_for(NSDictionary *attrs, int w, int h, OSType want) {
	NSMutableDictionary *a = attrs ? [attrs mutableCopy] : [NSMutableDictionary dictionary];
	a[(__bridge NSString *)kCVPixelBufferWidthKey] = @(w);
	a[(__bridge NSString *)kCVPixelBufferHeightKey] = @(h);
	if (want) a[(__bridge NSString *)kCVPixelBufferPixelFormatTypeKey] = @(want);
	if (!a[(__bridge NSString *)kCVPixelBufferIOSurfacePropertiesKey]) a[(__bridge NSString *)kCVPixelBufferIOSurfacePropertiesKey] = @{};
	CVPixelBufferPoolRef pool = NULL;
	if (CVPixelBufferPoolCreate(NULL, NULL, (__bridge CFDictionaryRef)a, &pool) != kCVReturnSuccess) return NULL;
	return pool;
}

// The processor for frames of this size: made on the first frame, and again when the size changes. Its ML
// model may take longer than a frame to load: the caller shows the frames as they are meanwhile.
static int ready(VitFi *fi, int w, int h) {
	if (fi->proc && fi->w == w && fi->h == h) return 1;
	drop_all(fi);
	Class C = cls(@"VTLowLatencyFrameInterpolationConfiguration");
	SEL init = NSSelectorFromString(@"initWithFrameWidth:frameHeight:numberOfInterpolatedFrames:");
	id cfg = ((id (*)(id, SEL, NSInteger, NSInteger, NSInteger))objc_msgSend)([C alloc], init, (NSInteger)w, (NSInteger)h, (NSInteger)1);
	if (!cfg) return 0;
	id proc = [[cls(@"VTFrameProcessor") alloc] init];
	NSError *err = nil;
	SEL start = NSSelectorFromString(@"startSessionWithConfiguration:error:");
	if (!proc || !((BOOL (*)(id, SEL, id, NSError **))objc_msgSend)(proc, start, cfg, &err)) return 0;
	// (the pictures it takes and gives: its own attributes; the source's in its first supported format)
	NSDictionary *src = ((id (*)(id, SEL))objc_msgSend)(cfg, NSSelectorFromString(@"sourcePixelBufferAttributes"));
	NSDictionary *dst = ((id (*)(id, SEL))objc_msgSend)(cfg, NSSelectorFromString(@"destinationPixelBufferAttributes"));
	fi->src_pool = pool_for(src, w, h, 0);
	fi->dst_pool = pool_for(dst, w, h, 0);
	// (and the decoder's own format, NV12 video range, for what goes back to the app)
	fi->out_pool = pool_for(nil, w, h, kCVPixelFormatType_420YpCbCr8BiPlanarVideoRange);
	if (!fi->src_pool || !fi->dst_pool || !fi->out_pool) {
		((void (*)(id, SEL))objc_msgSend)(proc, NSSelectorFromString(@"endSession"));
		drop_all(fi);
		return 0;
	}
	fi->cfg = (__bridge_retained void *)cfg;
	fi->proc = (__bridge_retained void *)proc;
	fi->w = w;
	fi->h = h;
	return 1;
}

// `px` in the processor's own source format (a copy from its pool, the decoder's buffers aside).
static CVPixelBufferRef as_source(VitFi *fi, CVPixelBufferRef px) {
	CVPixelBufferRef out = NULL;
	if (CVPixelBufferPoolCreatePixelBuffer(NULL, fi->src_pool, &out) != kCVReturnSuccess) return NULL;
	if (VTPixelTransferSessionTransferImage(fi->xfer, px, out) != noErr) { CVPixelBufferRelease(out); return NULL; }
	return out;
}

static id frame_of(CVPixelBufferRef px, int64_t pts) {
	SEL init = NSSelectorFromString(@"initWithBuffer:presentationTimeStamp:");
	return ((id (*)(id, SEL, CVPixelBufferRef, CMTime))objc_msgSend)([cls(@"VTFrameProcessorFrame") alloc], init, px, CMTimeMake(pts, 60));
}

// The picture halfway between `prev` and `next` (NV12, video range), retained: the caller releases it. NULL
// when it can't be made (not ready yet, an error): the caller shows the frame as it is.
CVPixelBufferRef vit_fi_mid(VitFi *fi, CVPixelBufferRef prev, CVPixelBufferRef next) {
	if (!fi || !prev || !next) return NULL;
	int w = (int)CVPixelBufferGetWidth(next), h = (int)CVPixelBufferGetHeight(next);
	if ((int)CVPixelBufferGetWidth(prev) != w || (int)CVPixelBufferGetHeight(prev) != h) return NULL;
	@autoreleasepool {
		if (!ready(fi, w, h)) return NULL;
		CVPixelBufferRef a = as_source(fi, prev), b = as_source(fi, next), d = NULL;
		CVPixelBufferRef out = NULL;
		if (a && b && CVPixelBufferPoolCreatePixelBuffer(NULL, fi->dst_pool, &d) == kCVReturnSuccess) {
			fi->pts += 2;
			id fa = frame_of(a, fi->pts - 2), fb = frame_of(b, fi->pts), fd = frame_of(d, fi->pts - 1);
			SEL pinit = NSSelectorFromString(@"initWithSourceFrame:previousFrame:interpolationPhase:destinationFrames:");
			id params = (fa && fb && fd) ? ((id (*)(id, SEL, id, id, id, id))objc_msgSend)([cls(@"VTLowLatencyFrameInterpolationParameters") alloc], pinit, fb, fa, @[ @0.5f ], @[ fd ]) : nil;
			NSError *err = nil;
			SEL process = NSSelectorFromString(@"processWithParameters:error:");
			id proc = (__bridge id)fi->proc;
			if (params && ((BOOL (*)(id, SEL, id, NSError **))objc_msgSend)(proc, process, params, &err)) {
				// (back in the decoder's format, for the app's copy)
				if (CVPixelBufferPoolCreatePixelBuffer(NULL, fi->out_pool, &out) == kCVReturnSuccess &&
						VTPixelTransferSessionTransferImage(fi->xfer, d, out) != noErr) {
					CVPixelBufferRelease(out);
					out = NULL;
				}
			}
		}
		if (a) CVPixelBufferRelease(a);
		if (b) CVPixelBufferRelease(b);
		if (d) CVPixelBufferRelease(d);
		return out;
	}
}

#endif
