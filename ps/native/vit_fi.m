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
#include <dispatch/dispatch.h>

// The init calls by objc_msgSend, typed as init is: it takes the object from alloc (consumed) and gives back a
// retained one, which can be nil or another object. Typed as plain calls, ARC would release the alloc'd object
// again after an init that had already let it go (returning nil): a crash on exactly the failures checked for.
typedef id (*InitIII)(__attribute__((ns_consumed)) id, SEL, NSInteger, NSInteger, NSInteger) __attribute__((ns_returns_retained));
typedef id (*InitFrame)(__attribute__((ns_consumed)) id, SEL, CVPixelBufferRef, CMTime) __attribute__((ns_returns_retained));
typedef id (*InitParams)(__attribute__((ns_consumed)) id, SEL, id, id, id, id) __attribute__((ns_returns_retained));

// The processor's stages: none, loading (its ML model, on a queue of its own), ready, or failed for this size.
enum { FI_NONE = 0, FI_LOADING = 1, FI_READY = 2, FI_FAILED = -1 };

typedef struct VitFi {
	void *cfg;    // (__bridge_retained) VTLowLatencyFrameInterpolationConfiguration
	void *proc;   // (__bridge_retained) VTFrameProcessor
	CVPixelBufferPoolRef src_pool, dst_pool, out_pool;
	VTPixelTransferSessionRef xfer;
	int w, h;     // the size loaded, or being loaded
	int stage;    // FI_*: written by the loading queue (release), read by the decoder (acquire)
	dispatch_group_t loads;
	int64_t pts;
} VitFi;

static int stage_of(VitFi *fi) { return __atomic_load_n(&fi->stage, __ATOMIC_ACQUIRE); }
static void set_stage(VitFi *fi, int s) { __atomic_store_n(&fi->stage, s, __ATOMIC_RELEASE); }

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
	fi->loads = dispatch_group_create();
	return fi;
}

void vit_fi_free(VitFi *fi) {
	if (!fi) return;
	// (a model still loading writes into fi: wait for it)
	if (fi->loads) dispatch_group_wait(fi->loads, DISPATCH_TIME_FOREVER);
	fi->loads = nil;
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

// Loads the processor for frames of this size (on the loading queue): its ML model can take far longer than a
// frame, so the decoder never waits for it; the frames are shown as they are meanwhile.
static void load(VitFi *fi, int w, int h) {
	@autoreleasepool {
		Class C = cls(@"VTLowLatencyFrameInterpolationConfiguration");
		SEL init = NSSelectorFromString(@"initWithFrameWidth:frameHeight:numberOfInterpolatedFrames:");
		id cfg = ((InitIII)objc_msgSend)([C alloc], init, (NSInteger)w, (NSInteger)h, (NSInteger)1);
		id proc = cfg ? [[cls(@"VTFrameProcessor") alloc] init] : nil;
		NSError *err = nil;
		SEL start = NSSelectorFromString(@"startSessionWithConfiguration:error:");
		if (!proc || !((BOOL (*)(id, SEL, id, NSError **))objc_msgSend)(proc, start, cfg, &err)) {
			set_stage(fi, FI_FAILED);
			return;
		}
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
			fi->w = w;
			fi->h = h;
			set_stage(fi, FI_FAILED);
			return;
		}
		fi->cfg = (__bridge_retained void *)cfg;
		fi->proc = (__bridge_retained void *)proc;
		set_stage(fi, FI_READY);
	}
}

// Whether the processor for this size is ready: starts loading it on the first frame, and again when the size
// changes (once the last load is done).
static int ready(VitFi *fi, int w, int h) {
	int st = stage_of(fi);
	if (st == FI_LOADING) return 0;
	if (fi->w == w && fi->h == h && st != FI_NONE) return st == FI_READY;
	drop_all(fi);
	fi->w = w;
	fi->h = h;
	set_stage(fi, FI_LOADING);
	dispatch_group_async(fi->loads, dispatch_get_global_queue(QOS_CLASS_UTILITY, 0), ^{ load(fi, w, h); });
	return 0;
}

// The processor's stage for the frames last given: 0 loading or not started, 1 ready, -1 failed (for good, at
// this size).
int vit_fi_state(VitFi *fi) {
	if (!fi) return -1;
	int st = stage_of(fi);
	return st == FI_READY ? 1 : st == FI_FAILED ? -1 : 0;
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
	return ((InitFrame)objc_msgSend)([cls(@"VTFrameProcessorFrame") alloc], init, px, CMTimeMake(pts, 60));
}

// The picture halfway between `prev` and `next` (NV12, video range), retained: the caller releases it. NULL
// when it can't be made (still loading: vit_fi_state 0; an error): the caller shows the frame as it is.
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
			id params = (fa && fb && fd) ? ((InitParams)objc_msgSend)([cls(@"VTLowLatencyFrameInterpolationParameters") alloc], pinit, fb, fa, @[ @0.5f ], @[ fd ]) : nil;
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
