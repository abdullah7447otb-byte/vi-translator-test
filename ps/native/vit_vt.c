// The PS5's H.264 or H.265 (HEVC) decoded by the Mac's own video decoder (VideoToolbox): on Apple
// silicon the media engine does the work, so the CPU is left for reading text and translating.
// Frames come in Annex B (start codes) as chiaki gives them; VideoToolbox wants each NAL unit
// with its length before it, and the parameter sets on their own (H.264: SPS 7 and PPS 8; HEVC:
// VPS 32, SPS 33, PPS 34).

#ifdef __APPLE__

#include <CoreFoundation/CoreFoundation.h>
#include <CoreMedia/CoreMedia.h>
#include <CoreVideo/CoreVideo.h>
#include <VideoToolbox/VideoToolbox.h>
#include <stdint.h>
#include <stdlib.h>
#include <string.h>

// motion smoothing (vit_fi.m)
typedef struct VitFi VitFi;
VitFi *vit_fi_new(void);
void vit_fi_free(VitFi *fi);
int vit_fi_supported(void);
CVPixelBufferRef vit_fi_mid(VitFi *fi, CVPixelBufferRef prev, CVPixelBufferRef next);

typedef struct VitVt {
	CMVideoFormatDescriptionRef fmt;
	VTDecompressionSessionRef session;
	// 1: HEVC, 0: H.264
	int hevc;
	uint8_t *vps, *sps, *pps;
	size_t vps_len, sps_len, pps_len;
	// the picture of the last decode (retained by the output callback)
	CVPixelBufferRef out;
	OSStatus out_status;
	// the planes copied out of it: valid until the next decode
	uint8_t *y, *uv;
	size_t y_cap, uv_cap;
	// the frame rewritten with lengths instead of start codes
	uint8_t *buf;
	size_t buf_cap;
	// motion smoothing (the setting; see smooth_step): on, the interpolator, the frame of the last decode,
	// whether it was its own frame again, the last frame with a picture of its own, the one shown
	int smooth;
	VitFi *fi;
	CVPixelBufferRef prev, base, shown;
	int prev_dup;
	// interpolations made, and their time (µs), for the log and for turning it off when too slow
	long mids, mid_us, failed;
} VitVt;

static void on_picture(void *user, void *frame_user, OSStatus status, VTDecodeInfoFlags flags, CVImageBufferRef image,
		CMTime pts, CMTime duration) {
	(void)frame_user; (void)flags; (void)pts; (void)duration;
	VitVt *vt = user;
	vt->out_status = status;
	if (status == noErr && image) {
		if (vt->out) CVPixelBufferRelease(vt->out);
		vt->out = CVPixelBufferRetain(image);
	}
}

static void drop_session(VitVt *vt) {
	if (vt->session) {
		VTDecompressionSessionWaitForAsynchronousFrames(vt->session);
		VTDecompressionSessionInvalidate(vt->session);
		CFRelease(vt->session);
		vt->session = NULL;
	}
	if (vt->fmt) {
		CFRelease(vt->fmt);
		vt->fmt = NULL;
	}
}

static int make_session(VitVt *vt) {
	drop_session(vt);
	if (vt->hevc) {
		const uint8_t *sets[3] = { vt->vps, vt->sps, vt->pps };
		const size_t sizes[3] = { vt->vps_len, vt->sps_len, vt->pps_len };
		if (CMVideoFormatDescriptionCreateFromHEVCParameterSets(NULL, 3, sets, sizes, 4, NULL, &vt->fmt) != noErr) return -2;
	} else {
		const uint8_t *sets[2] = { vt->sps, vt->pps };
		const size_t sizes[2] = { vt->sps_len, vt->pps_len };
		if (CMVideoFormatDescriptionCreateFromH264ParameterSets(NULL, 2, sets, sizes, 4, &vt->fmt) != noErr) return -2;
	}

	CFMutableDictionaryRef dest = CFDictionaryCreateMutable(NULL, 2, &kCFTypeDictionaryKeyCallBacks, &kCFTypeDictionaryValueCallBacks);
	int32_t pixel = kCVPixelFormatType_420YpCbCr8BiPlanarVideoRange;
	CFNumberRef pn = CFNumberCreate(NULL, kCFNumberSInt32Type, &pixel);
	CFDictionarySetValue(dest, kCVPixelBufferPixelFormatTypeKey, pn);
	CFRelease(pn);

	CFMutableDictionaryRef spec = CFDictionaryCreateMutable(NULL, 1, &kCFTypeDictionaryKeyCallBacks, &kCFTypeDictionaryValueCallBacks);
	CFDictionarySetValue(spec, kVTVideoDecoderSpecification_EnableHardwareAcceleratedVideoDecoder, kCFBooleanTrue);

	VTDecompressionOutputCallbackRecord cb = { on_picture, vt };
	OSStatus st = VTDecompressionSessionCreate(NULL, vt->fmt, spec, dest, &cb, &vt->session);
	CFRelease(dest);
	CFRelease(spec);
	if (st != noErr) {
		vt->session = NULL;
		return -3;
	}
	VTSessionSetProperty(vt->session, kVTDecompressionPropertyKey_RealTime, kCFBooleanTrue);
	return 0;
}

static int keep(uint8_t **dst, size_t *dst_len, const uint8_t *src, size_t len) {
	if (*dst && *dst_len == len && memcmp(*dst, src, len) == 0) return 0;
	uint8_t *n = realloc(*dst, len);
	if (!n) return -1;
	memcpy(n, src, len);
	*dst = n;
	*dst_len = len;
	return 1;
}

static int grow(uint8_t **p, size_t *cap, size_t want) {
	if (*cap >= want) return 0;
	uint8_t *n = realloc(*p, want);
	if (!n) return -1;
	*p = n;
	*cap = want;
	return 0;
}

VitVt *vit_vt_new(int hevc) {
	VitVt *vt = calloc(1, sizeof(VitVt));
	if (vt) vt->hevc = hevc ? 1 : 0;
	return vt;
}

// Whether the session decodes in hardware (the media engine): 1 yes, 0 no (software, as in a
// virtual machine), -1 not known yet (no session).
int vit_vt_hardware(VitVt *vt) {
	if (!vt || !vt->session) return -1;
	CFBooleanRef hw = NULL;
	if (VTSessionCopyProperty(vt->session, kVTDecompressionPropertyKey_UsingHardwareAcceleratedVideoDecoder, NULL, &hw) != noErr || !hw) return -1;
	int r = CFBooleanGetValue(hw) ? 1 : 0;
	CFRelease(hw);
	return r;
}

void vit_vt_free(VitVt *vt) {
	if (!vt) return;
	drop_session(vt);
	if (vt->out) CVPixelBufferRelease(vt->out);
	if (vt->prev) CVPixelBufferRelease(vt->prev);
	if (vt->base) CVPixelBufferRelease(vt->base);
	if (vt->shown) CVPixelBufferRelease(vt->shown);
	if (vt->fi) vit_fi_free(vt->fi);
	free(vt->vps); free(vt->sps); free(vt->pps); free(vt->y); free(vt->uv); free(vt->buf);
	free(vt);
}

// The same picture again: GTA VI's 30 frames a second sent as 60 (the luma on a coarse grid, a step of
// difference at most on average; a frame with its own picture differs by many).
static int same_picture(CVPixelBufferRef a, CVPixelBufferRef b) {
	if (CVPixelBufferGetWidth(a) != CVPixelBufferGetWidth(b) || CVPixelBufferGetHeight(a) != CVPixelBufferGetHeight(b)) return 0;
	if (CVPixelBufferLockBaseAddress(a, kCVPixelBufferLock_ReadOnly) != kCVReturnSuccess) return 0;
	if (CVPixelBufferLockBaseAddress(b, kCVPixelBufferLock_ReadOnly) != kCVReturnSuccess) {
		CVPixelBufferUnlockBaseAddress(a, kCVPixelBufferLock_ReadOnly);
		return 0;
	}
	const uint8_t *pa = CVPixelBufferGetBaseAddressOfPlane(a, 0), *pb = CVPixelBufferGetBaseAddressOfPlane(b, 0);
	size_t sa = CVPixelBufferGetBytesPerRowOfPlane(a, 0), sb = CVPixelBufferGetBytesPerRowOfPlane(b, 0);
	size_t w = CVPixelBufferGetWidthOfPlane(a, 0), h = CVPixelBufferGetHeightOfPlane(a, 0);
	long sum = 0, n = 0;
	if (pa && pb) {
		for (size_t y = h / 40; y < h; y += h / 20 + 1)
			for (size_t x = w / 80; x < w; x += w / 40 + 1) {
				int d = (int)pa[y * sa + x] - (int)pb[y * sb + x];
				sum += d < 0 ? -d : d;
				n++;
			}
	}
	CVPixelBufferUnlockBaseAddress(a, kCVPixelBufferLock_ReadOnly);
	CVPixelBufferUnlockBaseAddress(b, kCVPixelBufferLock_ReadOnly);
	return n > 0 && sum <= n;
}

static void put(CVPixelBufferRef *slot, CVPixelBufferRef px) {
	if (px) CVPixelBufferRetain(px);
	if (*slot) CVPixelBufferRelease(*slot);
	*slot = px;
}

// Motion smoothing: the frame shown is one frame behind the one decoded (16.7 ms at 60 frames a second),
// so a frame that is the one before it again can be replaced by one made between that one and the next:
// decoded A A' B B' C → shown A, mid(A,B), B, mid(B,C)… On any failure, or slower than 12 ms a frame on
// average, it turns itself off and the frames are shown as they come.
static CVPixelBufferRef smooth_step(VitVt *vt, CVPixelBufferRef cur) {
	if (!vt->fi) {
		vt->fi = vit_fi_new();
		if (!vt->fi) { vt->smooth = 0; return cur; }
	}
	if (!vt->prev) {
		put(&vt->prev, cur);
		put(&vt->base, cur);
		vt->prev_dup = 0;
		put(&vt->shown, cur);
		return vt->shown;
	}
	int cur_dup = same_picture(cur, vt->prev);
	CVPixelBufferRef out = vt->prev;
	CVPixelBufferRef mid = NULL;
	if (vt->prev_dup && !cur_dup && vt->base) {
		CFAbsoluteTime t0 = CFAbsoluteTimeGetCurrent();
		mid = vit_fi_mid(vt->fi, vt->base, cur);
		long us = (long)((CFAbsoluteTimeGetCurrent() - t0) * 1e6);
		if (mid) {
			vt->mids++;
			vt->mid_us += us;
			out = mid;
		} else if (++vt->failed > 30) {
			vt->smooth = 0;
		}
		// (too slow for 60 frames a second: off; after the first ones, while its model loads)
		if (vt->mids >= 60 && vt->mid_us / vt->mids > 12000) vt->smooth = 0;
	}
	put(&vt->shown, out);
	if (mid) CVPixelBufferRelease(mid);
	if (!cur_dup) put(&vt->base, cur);
	put(&vt->prev, cur);
	vt->prev_dup = cur_dup;
	return vt->shown;
}

// Motion smoothing on or off (the setting, read on every frame by the caller); off lets go of the frames kept.
void vit_vt_set_smooth(VitVt *vt, int on) {
	if (!on && vt->smooth) {
		put(&vt->prev, NULL);
		put(&vt->base, NULL);
		put(&vt->shown, NULL);
		vt->prev_dup = 0;
	}
	if (on && !vt->smooth && vt->failed > 30) return; // (turned off by failing: stays off for this stream)
	vt->smooth = on;
}

// Whether motion smoothing is working (1), was turned off by itself (-1), or is off (0); and its average time (µs).
int vit_vt_smooth_state(VitVt *vt, long *avg_us) {
	*avg_us = vt->mids ? vt->mid_us / vt->mids : 0;
	if (vt->smooth) return 1;
	return (vt->failed > 30 || (vt->mids >= 60 && *avg_us > 12000)) ? -1 : 0;
}

int vit_vt_smooth_supported(void) { return vit_fi_supported(); }

// Decodes one frame. 1: a picture (NV12: *y and *uv, valid until the next call), 0: none yet
// (no parameter sets seen: a key frame is needed), below 0: VideoToolbox failed.
int vit_vt_decode(VitVt *vt, const uint8_t *data, size_t len, const uint8_t **y, const uint8_t **uv,
		int *w, int *h, int *y_stride, int *uv_stride) {
	if (grow(&vt->buf, &vt->buf_cap, len + 64) < 0) return -1;
	size_t out = 0;
	int params_changed = 0;
	// the NAL units: from after one start code (00 00 01) to the next; the zero before a
	// four-byte start code, and any trailing zeros, are left off the unit before it
	size_t i = 0;
	while (i + 3 <= len && !(data[i] == 0 && data[i + 1] == 0 && data[i + 2] == 1)) i++;
	while (i + 3 <= len) {
		size_t start = i + 3, end = start;
		while (end + 3 <= len && !(data[end] == 0 && data[end + 1] == 0 && data[end + 2] == 1)) end++;
		if (end + 3 > len) end = len;
		size_t n = end - start;
		while (n > 0 && data[start + n - 1] == 0) n--;
		if (n > 0) {
			// (the unit's type, and which parameter set it is: H.264 7 SPS, 8 PPS, 9 delimiter;
			// HEVC 32 VPS, 33 SPS, 34 PPS, 35 delimiter)
			int type = vt->hevc ? (data[start] >> 1) & 0x3f : data[start] & 0x1f;
			int is_vps = vt->hevc && type == 32;
			int is_sps = vt->hevc ? type == 33 : type == 7;
			int is_pps = vt->hevc ? type == 34 : type == 8;
			int is_aud = vt->hevc ? type == 35 : type == 9;
			if (is_vps) {
				int r = keep(&vt->vps, &vt->vps_len, data + start, n);
				if (r < 0) return -1;
				params_changed |= r;
			} else if (is_sps) {
				int r = keep(&vt->sps, &vt->sps_len, data + start, n);
				if (r < 0) return -1;
				params_changed |= r;
			} else if (is_pps) {
				int r = keep(&vt->pps, &vt->pps_len, data + start, n);
				if (r < 0) return -1;
				params_changed |= r;
			} else if (!is_aud) {
				if (grow(&vt->buf, &vt->buf_cap, out + n + 4) < 0) return -1;
				vt->buf[out] = (uint8_t)(n >> 24); vt->buf[out + 1] = (uint8_t)(n >> 16);
				vt->buf[out + 2] = (uint8_t)(n >> 8); vt->buf[out + 3] = (uint8_t)n;
				memcpy(vt->buf + out + 4, data + start, n);
				out += n + 4;
			}
		}
		i = end;
	}
	if (!vt->sps || !vt->pps || (vt->hevc && !vt->vps)) return 0;
	if (params_changed || !vt->session) {
		int r = make_session(vt);
		if (r < 0) return r;
	}
	if (out == 0) return 0;

	CMBlockBufferRef block = NULL;
	if (CMBlockBufferCreateWithMemoryBlock(NULL, vt->buf, out, kCFAllocatorNull, NULL, 0, out, 0, &block) != noErr) return -4;
	CMSampleBufferRef sample = NULL;
	const size_t sizes[1] = { out };
	OSStatus st = CMSampleBufferCreateReady(NULL, block, vt->fmt, 1, 0, NULL, 1, sizes, &sample);
	CFRelease(block);
	if (st != noErr) return -5;

	if (vt->out) { CVPixelBufferRelease(vt->out); vt->out = NULL; }
	vt->out_status = noErr;
	VTDecodeInfoFlags info = 0;
	st = VTDecompressionSessionDecodeFrame(vt->session, sample, 0, NULL, &info);
	VTDecompressionSessionWaitForAsynchronousFrames(vt->session);
	CFRelease(sample);
	if (st == kVTInvalidSessionErr) {
		// (after sleep the session is gone: made again with the next key frame)
		drop_session(vt);
		return -6;
	}
	if (st != noErr) return -7;
	if (vt->out_status != noErr) return -8;
	if (!vt->out) return 0;

	CVPixelBufferRef px = vt->smooth ? smooth_step(vt, vt->out) : vt->out;
	if (!px) px = vt->out;
	if (CVPixelBufferLockBaseAddress(px, kCVPixelBufferLock_ReadOnly) != kCVReturnSuccess) return -9;
	size_t pw = CVPixelBufferGetWidthOfPlane(px, 0), ph = CVPixelBufferGetHeightOfPlane(px, 0);
	size_t ys = CVPixelBufferGetBytesPerRowOfPlane(px, 0), uvs = CVPixelBufferGetBytesPerRowOfPlane(px, 1);
	size_t uvh = CVPixelBufferGetHeightOfPlane(px, 1);
	const uint8_t *py = CVPixelBufferGetBaseAddressOfPlane(px, 0), *puv = CVPixelBufferGetBaseAddressOfPlane(px, 1);
	int ok = py && puv && grow(&vt->y, &vt->y_cap, ys * ph) == 0 && grow(&vt->uv, &vt->uv_cap, uvs * uvh) == 0;
	if (ok) {
		memcpy(vt->y, py, ys * ph);
		memcpy(vt->uv, puv, uvs * uvh);
	}
	CVPixelBufferUnlockBaseAddress(px, kCVPixelBufferLock_ReadOnly);
	if (!ok) return -10;
	*y = vt->y; *uv = vt->uv;
	*w = (int)pw; *h = (int)ph; *y_stride = (int)ys; *uv_stride = (int)uvs;
	return 1;
}

#endif
