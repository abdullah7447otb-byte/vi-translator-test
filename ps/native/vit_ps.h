// SPDX-License-Identifier: AGPL-3.0-only
// VI Translator's small C face of chiaki-ng's library: plain functions and structs that Rust binds
// by hand, so the Rust side never needs chiaki's own (large, changing) structs.
#ifndef VIT_PS_H
#define VIT_PS_H

#include <stdbool.h>
#include <stddef.h>
#include <stdint.h>

#ifdef __cplusplus
extern "C" {
#endif

/** chiaki's log lines: level (CHIAKI_LOG_ERROR 1 … DEBUG 16) and text. */
typedef void (*VitLogCb)(int level, const char *msg, void *user);

/** Once per run of the app, before anything else. 0 = fine. */
int vit_ps_init(void);
const char *vit_ps_error_string(int err);

// ---------------------------------------------------------------------------- finding consoles

typedef struct VitPsHost
{
	int state; // 0 unknown, 1 ready (on), 2 standby (rest mode)
	bool ps5;
	const char *addr;
	const char *name;
	const char *id;
	const char *system_version;
} VitPsHost;

typedef void (*VitHostsCb)(const VitPsHost *hosts, size_t count, void *user);

/**
 * Looks for PlayStations on the home network until stopped; cb on its own thread. Asks everyone
 * (255.255.255.255) and also each of `extra` (IPv4, a.b.c.d as (a << 24) | …): the networks' own
 * broadcast addresses (a PC with several network adapters sends 255.255.255.255 out of one only)
 * and known consoles.
 */
void *vit_discovery_start(const uint32_t *extra, size_t extra_count, VitHostsCb cb, void *user, VitLogCb log, void *log_user, int *err);
void vit_discovery_stop(void *discovery);

/** Wakes a PS5 in rest mode: its address and the number from its registration key. */
int vit_wakeup(const char *host, uint64_t credential);

// ---------------------------------------------------------------------------- registering

typedef struct VitPsRegistered
{
	char nickname[0x20];
	uint8_t mac[6];
	char regist_key[16];
	uint8_t rp_key[16];
} VitPsRegistered;

/** result: 0 registered (reg filled), 1 failed, 2 cancelled; on chiaki's thread. */
typedef void (*VitRegistCb)(int result, const VitPsRegistered *reg, void *user);

/** Registers with the PS5 at host, with the PSN account ID (8 bytes) and the PIN it shows. */
void *vit_regist_start(const char *host, const uint8_t account_id[8], uint32_t pin, VitRegistCb cb, void *user, VitLogCb log, void *log_user, int *err);
/** Stops (if still running) and frees. */
void vit_regist_free(void *regist);

// ---------------------------------------------------------------------------- playing

enum
{
	VIT_EVENT_CONNECTED = 1,
	VIT_EVENT_LOGIN_PIN = 2, // arg: 1 if the PIN entered before was wrong
	VIT_EVENT_QUIT = 3, // arg: 1 if an error; text: why
	VIT_EVENT_RUMBLE = 4, // arg: left << 8 | right
	VIT_EVENT_NICKNAME = 5, // text
};

typedef struct VitPsCallbacks
{
	void *user;
	VitLogCb log;
	void (*event)(int type, int arg, const char *text, void *user);
	/** One encoded frame (H.264 or H.265 access unit). Return false if it could not be decoded. */
	/* frame_index: the frame's number in the stream (for vit_session_report_corrupt) */
	bool (*video)(const uint8_t *buf, size_t size, int frames_lost, bool recovered, int frame_index, void *user);
	void (*audio_header)(int channels, int rate, int frame_size, void *user);
	/** One Opus packet. */
	void (*audio_frame)(const uint8_t *buf, size_t size, void *user);
} VitPsCallbacks;

/** codec: 0 H.264, 1 H.265. Starts connecting; events tell the rest. NULL on failure (err set). */
void *vit_session_start(const char *host, const char regist_key[16], const uint8_t rp_key[16], unsigned width, unsigned height, unsigned fps,
		unsigned bitrate, int codec, const VitPsCallbacks *cb, int *err);
void vit_session_controller(void *session, uint32_t buttons, uint8_t l2, uint8_t r2, int16_t lx, int16_t ly, int16_t rx, int16_t ry);
void vit_session_login_pin(void *session, const char *pin);
void vit_session_request_idr(void *session);
/* Tells the PS5 that frames start..end (their frame_index) could not be decoded, as chiaki does for
 * a frame its video callback turned down: the PS5 then sends a picture that doesn't build on them. */
void vit_session_report_corrupt(void *session, int start, int end);
/** Ends the stream and frees it (waits for chiaki's threads). */
void vit_session_free(void *session);

#ifdef __cplusplus
}
#endif

#endif
