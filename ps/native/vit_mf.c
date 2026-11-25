// The PS5's H.264 decoded by Windows (Media Foundation's H.264 decoder): with a Direct3D 11
// device the graphics card decodes (DXVA), so a weak laptop's CPU is left for reading text.
// Without one (no card that can, or a remote desktop) Windows decodes on the CPU, and the app's
// own decoder (OpenH264) is the one to use instead: vit_mf_hardware() tells which.

#ifdef _WIN32

#define COBJMACROS
#define WIN32_LEAN_AND_MEAN
#include <windows.h>
#include <initguid.h>
#include <d3d11.h>
#include <d3d10.h>
#include <mfapi.h>
#include <mferror.h>
#include <mfidl.h>
#include <mftransform.h>
#include <stdint.h>
#include <stdlib.h>
#include <string.h>

// (wmcodecdsp.h's, here so the uuid library isn't needed)
DEFINE_GUID(VIT_CLSID_CMSH264DecoderMFT, 0x62CE7E72, 0x4C71, 0x4d20, 0xB1, 0x5D, 0x45, 0x28, 0x31, 0xA8, 0x7D, 0x9D);

typedef struct VitMf {
	IMFTransform *t;
	ID3D11Device *dev;
	IMFDXGIDeviceManager *mgr;
	int hardware;
	int provides_samples;
	DWORD out_size;
	UINT32 w, h;        // the picture shown (the display aperture)
	LONGLONG ts;
	uint8_t *y, *uv;
	size_t y_cap, uv_cap;
	int com;
} VitMf;

static int grow(uint8_t **p, size_t *cap, size_t want) {
	if (*cap >= want) return 0;
	uint8_t *n = realloc(*p, want);
	if (!n) return -1;
	*p = n;
	*cap = want;
	return 0;
}

// NV12 out: the first type the decoder offers that is NV12
static HRESULT set_output(VitMf *m) {
	for (DWORD i = 0;; i++) {
		IMFMediaType *mt = NULL;
		HRESULT hr = IMFTransform_GetOutputAvailableType(m->t, 0, i, &mt);
		if (FAILED(hr)) return hr;
		GUID sub;
		if (SUCCEEDED(IMFMediaType_GetGUID(mt, &MF_MT_SUBTYPE, &sub)) && IsEqualGUID(&sub, &MFVideoFormat_NV12)) {
			hr = IMFTransform_SetOutputType(m->t, 0, mt, 0);
			if (SUCCEEDED(hr)) {
				UINT64 size = 0;
				IMFMediaType_GetUINT64(mt, &MF_MT_FRAME_SIZE, &size);
				m->w = (UINT32)(size >> 32);
				m->h = (UINT32)size;
				MFVideoArea area;
				UINT32 got = 0;
				if (SUCCEEDED(IMFMediaType_GetBlob(mt, &MF_MT_MINIMUM_DISPLAY_APERTURE, (UINT8 *)&area, sizeof area, &got)) && got == sizeof area
						&& area.Area.cx > 0 && area.Area.cy > 0) {
					m->w = (UINT32)area.Area.cx;
					m->h = (UINT32)area.Area.cy;
				}
				MFT_OUTPUT_STREAM_INFO info;
				if (SUCCEEDED(IMFTransform_GetOutputStreamInfo(m->t, 0, &info))) {
					m->provides_samples = (info.dwFlags & (MFT_OUTPUT_STREAM_PROVIDES_SAMPLES | MFT_OUTPUT_STREAM_CAN_PROVIDE_SAMPLES)) != 0;
					m->out_size = info.cbSize;
				}
			}
			IMFMediaType_Release(mt);
			return hr;
		}
		IMFMediaType_Release(mt);
	}
}

static void try_d3d(VitMf *m) {
	IMFAttributes *attrs = NULL;
	UINT32 aware = 0;
	if (FAILED(IMFTransform_GetAttributes(m->t, &attrs)) || !attrs) return;
	IMFAttributes_GetUINT32(attrs, &MF_SA_D3D11_AWARE, &aware);
	IMFAttributes_Release(attrs);
	if (!aware) return;
	D3D_FEATURE_LEVEL levels[] = { D3D_FEATURE_LEVEL_11_1, D3D_FEATURE_LEVEL_11_0, D3D_FEATURE_LEVEL_10_1, D3D_FEATURE_LEVEL_10_0 };
	if (FAILED(D3D11CreateDevice(NULL, D3D_DRIVER_TYPE_HARDWARE, NULL, D3D11_CREATE_DEVICE_VIDEO_SUPPORT, levels, 4, D3D11_SDK_VERSION, &m->dev, NULL, NULL))) {
		m->dev = NULL;
		return;
	}
	ID3D10Multithread *mt = NULL;
	if (SUCCEEDED(ID3D11Device_QueryInterface(m->dev, &IID_ID3D10Multithread, (void **)&mt)) && mt) {
		ID3D10Multithread_SetMultithreadProtected(mt, TRUE);
		ID3D10Multithread_Release(mt);
	}
	UINT token = 0;
	if (FAILED(MFCreateDXGIDeviceManager(&token, &m->mgr))) goto fail;
	if (FAILED(IMFDXGIDeviceManager_ResetDevice(m->mgr, (IUnknown *)m->dev, token))) goto fail;
	if (FAILED(IMFTransform_ProcessMessage(m->t, MFT_MESSAGE_SET_D3D_MANAGER, (ULONG_PTR)m->mgr))) goto fail;
	m->hardware = 1;
	return;
fail:
	if (m->mgr) { IMFDXGIDeviceManager_Release(m->mgr); m->mgr = NULL; }
	if (m->dev) { ID3D11Device_Release(m->dev); m->dev = NULL; }
}

void vit_mf_free(VitMf *m);

// software: 1 lets Windows decode on the CPU (only the tests ask for that)
VitMf *vit_mf_new(int software) {
	VitMf *m = calloc(1, sizeof(VitMf));
	if (!m) return NULL;
	HRESULT co = CoInitializeEx(NULL, COINIT_MULTITHREADED);
	m->com = SUCCEEDED(co);
	if (FAILED(MFStartup(MF_VERSION, MFSTARTUP_LITE))) { free(m); return NULL; }
	if (FAILED(CoCreateInstance(&VIT_CLSID_CMSH264DecoderMFT, NULL, CLSCTX_INPROC_SERVER, &IID_IMFTransform, (void **)&m->t)) || !m->t) {
		m->t = NULL;
		vit_mf_free(m);
		return NULL;
	}
	IMFAttributes *attrs = NULL;
	if (SUCCEEDED(IMFTransform_GetAttributes(m->t, &attrs)) && attrs) {
		// (a picture out for each frame in, not held back for reordering)
		IMFAttributes_SetUINT32(attrs, &MF_LOW_LATENCY, TRUE);
		IMFAttributes_Release(attrs);
	}
	if (!software) try_d3d(m);
	if (!software && !m->hardware) { vit_mf_free(m); return NULL; }

	IMFMediaType *in = NULL;
	if (FAILED(MFCreateMediaType(&in))) { vit_mf_free(m); return NULL; }
	IMFMediaType_SetGUID(in, &MF_MT_MAJOR_TYPE, &MFMediaType_Video);
	IMFMediaType_SetGUID(in, &MF_MT_SUBTYPE, &MFVideoFormat_H264);
	IMFMediaType_SetUINT32(in, &MF_MT_INTERLACE_MODE, MFVideoInterlace_Progressive);
	HRESULT hr = IMFTransform_SetInputType(m->t, 0, in, 0);
	IMFMediaType_Release(in);
	if (FAILED(hr) || FAILED(set_output(m))) { vit_mf_free(m); return NULL; }
	IMFTransform_ProcessMessage(m->t, MFT_MESSAGE_NOTIFY_BEGIN_STREAMING, 0);
	IMFTransform_ProcessMessage(m->t, MFT_MESSAGE_NOTIFY_START_OF_STREAM, 0);
	return m;
}

int vit_mf_hardware(const VitMf *m) {
	return m && m->hardware;
}

void vit_mf_free(VitMf *m) {
	if (!m) return;
	if (m->t) IMFTransform_Release(m->t);
	if (m->mgr) IMFDXGIDeviceManager_Release(m->mgr);
	if (m->dev) ID3D11Device_Release(m->dev);
	MFShutdown();
	if (m->com) CoUninitialize();
	free(m->y); free(m->uv);
	free(m);
}

// the picture in a decoded sample, copied out as NV12
static int copy_out(VitMf *m, IMFSample *s, const uint8_t **y, const uint8_t **uv, int *w, int *h, int *ys, int *uvs) {
	IMFMediaBuffer *b = NULL;
	if (FAILED(IMFSample_GetBufferByIndex(s, 0, &b)) || !b) return -20;
	IMF2DBuffer *b2 = NULL;
	BYTE *p = NULL;
	LONG pitch = 0;
	DWORD len = 0;
	int r = -21;
	if (SUCCEEDED(IMFMediaBuffer_QueryInterface(b, &IID_IMF2DBuffer, (void **)&b2)) && b2) {
		if (SUCCEEDED(IMF2DBuffer_Lock2D(b2, &p, &pitch)) && p && pitch > 0) {
			DWORD clen = 0;
			IMF2DBuffer_GetContiguousLength(b2, &clen);
			// (the planes' height: the buffer's, which the decoder rounds up to 16 rows)
			UINT32 rows = clen ? (UINT32)(clen / ((DWORD)pitch * 3 / 2)) : m->h;
			if (rows < m->h) rows = m->h;
			size_t ysz = (size_t)pitch * m->h, uvsz = (size_t)pitch * ((m->h + 1) / 2);
			if (grow(&m->y, &m->y_cap, ysz) == 0 && grow(&m->uv, &m->uv_cap, uvsz) == 0) {
				memcpy(m->y, p, ysz);
				memcpy(m->uv, p + (size_t)pitch * rows, uvsz);
				r = 1;
			}
			IMF2DBuffer_Unlock2D(b2);
		}
		IMF2DBuffer_Release(b2);
	} else if (SUCCEEDED(IMFMediaBuffer_Lock(b, &p, NULL, &len)) && p) {
		pitch = (LONG)m->w;
		UINT32 rows = len ? (UINT32)(len / (m->w * 3 / 2)) : m->h;
		if (rows < m->h) rows = m->h;
		size_t ysz = (size_t)pitch * m->h, uvsz = (size_t)pitch * ((m->h + 1) / 2);
		if ((size_t)pitch * rows + uvsz <= len && grow(&m->y, &m->y_cap, ysz) == 0 && grow(&m->uv, &m->uv_cap, uvsz) == 0) {
			memcpy(m->y, p, ysz);
			memcpy(m->uv, p + (size_t)pitch * rows, uvsz);
			r = 1;
		}
		IMFMediaBuffer_Unlock(b);
	}
	IMFMediaBuffer_Release(b);
	if (r == 1) {
		*y = m->y; *uv = m->uv; *w = (int)m->w; *h = (int)m->h; *ys = (int)pitch; *uvs = (int)pitch;
	}
	return r;
}

// One frame in (Annex B, as chiaki gives it). 1: a picture (NV12, valid until the next call),
// 0: none yet, below 0: Windows failed.
int vit_mf_decode(VitMf *m, const uint8_t *data, size_t len, const uint8_t **y, const uint8_t **uv,
		int *w, int *h, int *y_stride, int *uv_stride) {
	IMFMediaBuffer *buf = NULL;
	if (FAILED(MFCreateMemoryBuffer((DWORD)len, &buf))) return -1;
	BYTE *p = NULL;
	if (FAILED(IMFMediaBuffer_Lock(buf, &p, NULL, NULL))) { IMFMediaBuffer_Release(buf); return -1; }
	memcpy(p, data, len);
	IMFMediaBuffer_Unlock(buf);
	IMFMediaBuffer_SetCurrentLength(buf, (DWORD)len);
	IMFSample *in = NULL;
	if (FAILED(MFCreateSample(&in))) { IMFMediaBuffer_Release(buf); return -1; }
	IMFSample_AddBuffer(in, buf);
	IMFMediaBuffer_Release(buf);
	IMFSample_SetSampleTime(in, m->ts);
	IMFSample_SetSampleDuration(in, 166667);
	m->ts += 166667;
	HRESULT hr = IMFTransform_ProcessInput(m->t, 0, in, 0);
	IMFSample_Release(in);
	if (FAILED(hr) && hr != MF_E_NOTACCEPTING) return -2;

	int got = 0;
	for (int tries = 0; tries < 4; tries++) {
		MFT_OUTPUT_DATA_BUFFER out = { 0 };
		IMFSample *mine = NULL;
		if (!m->provides_samples) {
			IMFMediaBuffer *ob = NULL;
			DWORD size = m->out_size ? m->out_size : m->w * m->h * 3 / 2 + 65536;
			if (FAILED(MFCreateMemoryBuffer(size, &ob))) return -3;
			if (FAILED(MFCreateSample(&mine))) { IMFMediaBuffer_Release(ob); return -3; }
			IMFSample_AddBuffer(mine, ob);
			IMFMediaBuffer_Release(ob);
			out.pSample = mine;
		}
		DWORD status = 0;
		hr = IMFTransform_ProcessOutput(m->t, 0, 1, &out, &status);
		if (out.pEvents) IMFCollection_Release(out.pEvents);
		if (hr == MF_E_TRANSFORM_NEED_MORE_INPUT) {
			if (mine) IMFSample_Release(mine);
			break;
		}
		if (hr == MF_E_TRANSFORM_STREAM_CHANGE) {
			if (mine) IMFSample_Release(mine);
			if (FAILED(set_output(m))) return -4;
			continue;
		}
		if (FAILED(hr)) {
			if (mine) IMFSample_Release(mine);
			else if (out.pSample) IMFSample_Release(out.pSample);
			return -5;
		}
		if (out.pSample) {
			int r = copy_out(m, out.pSample, y, uv, w, h, y_stride, uv_stride);
			if (!mine) IMFSample_Release(out.pSample);
			if (r < 0) { if (mine) IMFSample_Release(mine); return r; }
			got = 1;
		}
		if (mine) IMFSample_Release(mine);
	}
	return got;
}

#endif
