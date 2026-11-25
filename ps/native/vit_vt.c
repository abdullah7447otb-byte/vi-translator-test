// The PS5's H.264 decoded by the Mac's own video decoder (VideoToolbox): on Apple silicon the
// media engine does the work, so the CPU is left for reading text and translating.
// Frames come in Annex B (start codes) as chiaki gives them; VideoToolbox wants each NAL unit
// with its length before it, and the sequence and picture parameters on their own.

#ifdef __APPLE__

#include <CoreFoundation/CoreFoundation.h>
#include <CoreMedia/CoreMedia.h>
#include <CoreVideo/CoreVideo.h>
#include <VideoToolbox/VideoToolbox.h>
#include <stdint.h>
#include <stdlib.h>
#include <string.h>

typedef struct VitVt {
	CMVideoFormatDescriptionRef fmt;
	VTDecompressionSessionRef session;
	uint8_t *sps, *pps;
	size_t sps_len, pps_len;
	// the picture of the last decode (retained by the output callback)
	CVPixelBufferRef out;
	OSStatus out_status;
	// the planes copied out of it: valid until the next decode
	uint8_t *y, *uv;
	size_t y_cap, uv_cap;
	// the frame rewritten with lengths instead of start codes
	uint8_t *buf;
	size_t buf_cap;
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
	const uint8_t *sets[2] = { vt->sps, vt->pps };
	const size_t sizes[2] = { vt->sps_len, vt->pps_len };
	if (CMVideoFormatDescriptionCreateFromH264ParameterSets(NULL, 2, sets, sizes, 4, &vt->fmt) != noErr) return -2;

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

VitVt *vit_vt_new(void) {
	return calloc(1, sizeof(VitVt));
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
	free(vt->sps); free(vt->pps); free(vt->y); free(vt->uv); free(vt->buf);
	free(vt);
}

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
			int type = data[start] & 0x1f;
			if (type == 7) {
				int r = keep(&vt->sps, &vt->sps_len, data + start, n);
				if (r < 0) return -1;
				params_changed |= r;
			} else if (type == 8) {
				int r = keep(&vt->pps, &vt->pps_len, data + start, n);
				if (r < 0) return -1;
				params_changed |= r;
			} else if (type != 9) {
				if (grow(&vt->buf, &vt->buf_cap, out + n + 4) < 0) return -1;
				vt->buf[out] = (uint8_t)(n >> 24); vt->buf[out + 1] = (uint8_t)(n >> 16);
				vt->buf[out + 2] = (uint8_t)(n >> 8); vt->buf[out + 3] = (uint8_t)n;
				memcpy(vt->buf + out + 4, data + start, n);
				out += n + 4;
			}
		}
		i = end;
	}
	if (!vt->sps || !vt->pps) return 0;
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

	CVPixelBufferRef px = vt->out;
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
