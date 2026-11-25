// SPDX-License-Identifier: AGPL-3.0-only
// VI Translator's small C face of chiaki-ng's library (see vit_ps.h). Its use of chiaki follows
// chiaki-ng's own Android app (android/app/src/main/cpp/chiaki-jni.c).

#include "vit_ps.h"

#include <chiaki/common.h>
#include <chiaki/controller.h>
#include <chiaki/discovery.h>
#include <chiaki/discoveryservice.h>
#include <chiaki/log.h>
#include <chiaki/regist.h>
#include <chiaki/session.h>
#include <chiaki/streamconnection.h>
#include <chiaki/videoreceiver.h>

#include <stdlib.h>
#include <string.h>

#ifdef _WIN32
#include <winsock2.h>
#include <ws2tcpip.h>
#else
#include <arpa/inet.h>
#include <netinet/in.h>
#include <sys/socket.h>
#endif

#define DISCOVERY_PORT 987

int vit_ps_init(void)
{
	return (int)chiaki_lib_init();
}

const char *vit_ps_error_string(int err)
{
	return chiaki_error_string((ChiakiErrorCode)err);
}

typedef struct
{
	VitLogCb cb;
	void *user;
} LogTarget;

static void log_cb(ChiakiLogLevel level, const char *msg, void *user)
{
	LogTarget *t = user;
	if(t && t->cb)
		t->cb((int)level, msg, t->user);
}

static void log_init(ChiakiLog *log, LogTarget *target, VitLogCb cb, void *user)
{
	target->cb = cb;
	target->user = user;
	chiaki_log_init(log, CHIAKI_LOG_ALL & ~CHIAKI_LOG_VERBOSE & ~CHIAKI_LOG_DEBUG, log_cb, target);
}

// ---------------------------------------------------------------------------- finding consoles

typedef struct
{
	ChiakiLog log;
	LogTarget log_target;
	ChiakiDiscoveryService service;
	VitHostsCb cb;
	void *user;
} Discovery;

static void discovery_cb(ChiakiDiscoveryHost *hosts, size_t count, void *user)
{
	Discovery *d = user;
	VitPsHost *out = count ? calloc(count, sizeof(VitPsHost)) : NULL;
	if(count && !out)
		return;
	for(size_t i = 0; i < count; i++)
	{
		ChiakiDiscoveryHost *h = &hosts[i];
		out[i].state = h->state == CHIAKI_DISCOVERY_HOST_STATE_READY ? 1 : h->state == CHIAKI_DISCOVERY_HOST_STATE_STANDBY ? 2 : 0;
		out[i].ps5 = chiaki_discovery_host_is_ps5(h);
		out[i].addr = h->host_addr;
		out[i].name = h->host_name;
		out[i].id = h->host_id;
		out[i].system_version = h->system_version;
	}
	d->cb(out, count, d->user);
	free(out);
}

void *vit_discovery_start(const uint32_t *extra, size_t extra_count, VitHostsCb cb, void *user, VitLogCb log, void *log_user, int *err)
{
	Discovery *d = calloc(1, sizeof(Discovery));
	if(!d)
	{
		*err = CHIAKI_ERR_MEMORY;
		return NULL;
	}
	log_init(&d->log, &d->log_target, log, log_user);
	d->cb = cb;
	d->user = user;

	struct sockaddr_in *addr = calloc(1, sizeof(struct sockaddr_storage));
	if(!addr)
	{
		free(d);
		*err = CHIAKI_ERR_MEMORY;
		return NULL;
	}
	addr->sin_family = AF_INET;
	addr->sin_port = htons(DISCOVERY_PORT);
	addr->sin_addr.s_addr = htonl(INADDR_BROADCAST);

	ChiakiDiscoveryServiceOptions options = { 0 };
	options.hosts_max = 8;
	options.host_drop_pings = 3;
	options.ping_ms = 500;
	options.ping_initial_ms = 500;
	options.send_addr = (struct sockaddr_storage *)addr;
	options.send_addr_size = sizeof(struct sockaddr_in);
	options.cb = discovery_cb;
	options.cb_user = d;
	struct sockaddr_storage *more = extra_count ? calloc(extra_count, sizeof(struct sockaddr_storage)) : NULL;
	if(more)
	{
		for(size_t i = 0; i < extra_count; i++)
		{
			struct sockaddr_in *a = (struct sockaddr_in *)&more[i];
			a->sin_family = AF_INET;
			a->sin_addr.s_addr = htonl(extra[i]);
		}
		options.broadcast_addrs = more;
		options.broadcast_num = extra_count;
	}

	ChiakiErrorCode e = chiaki_discovery_service_init(&d->service, &options, &d->log);
	free(addr); // (the service keeps its own copies)
	free(more);
	*err = (int)e;
	if(e != CHIAKI_ERR_SUCCESS)
	{
		free(d);
		return NULL;
	}
	return d;
}

void vit_discovery_stop(void *discovery)
{
	Discovery *d = discovery;
	if(!d)
		return;
	chiaki_discovery_service_fini(&d->service);
	free(d);
}

int vit_wakeup(const char *host, uint64_t credential)
{
	return (int)chiaki_discovery_wakeup(NULL, NULL, host, credential, true);
}

// ---------------------------------------------------------------------------- registering

typedef struct
{
	ChiakiLog log;
	LogTarget log_target;
	ChiakiRegist regist;
	VitRegistCb cb;
	void *user;
} Regist;

static void regist_cb(ChiakiRegistEvent *event, void *user)
{
	Regist *r = user;
	switch(event->type)
	{
		case CHIAKI_REGIST_EVENT_TYPE_FINISHED_SUCCESS:
		{
			ChiakiRegisteredHost *h = event->registered_host;
			VitPsRegistered reg = { 0 };
			memcpy(reg.nickname, h->server_nickname, sizeof(reg.nickname));
			reg.nickname[sizeof(reg.nickname) - 1] = 0;
			memcpy(reg.mac, h->server_mac, sizeof(reg.mac));
			memcpy(reg.regist_key, h->rp_regist_key, sizeof(reg.regist_key));
			memcpy(reg.rp_key, h->rp_key, sizeof(reg.rp_key));
			r->cb(0, &reg, r->user);
			break;
		}
		case CHIAKI_REGIST_EVENT_TYPE_FINISHED_FAILED:
			r->cb(1, NULL, r->user);
			break;
		case CHIAKI_REGIST_EVENT_TYPE_FINISHED_CANCELED:
			r->cb(2, NULL, r->user);
			break;
	}
}

void *vit_regist_start(const char *host, const uint8_t account_id[8], uint32_t pin, VitRegistCb cb, void *user, VitLogCb log, void *log_user, int *err)
{
	Regist *r = calloc(1, sizeof(Regist));
	if(!r)
	{
		*err = CHIAKI_ERR_MEMORY;
		return NULL;
	}
	log_init(&r->log, &r->log_target, log, log_user);
	r->cb = cb;
	r->user = user;

	ChiakiRegistInfo info = { 0 };
	info.target = CHIAKI_TARGET_PS5_1;
	info.host = host;
	info.broadcast = false;
	info.psn_online_id = NULL;
	memcpy(info.psn_account_id, account_id, sizeof(info.psn_account_id));
	info.pin = pin;

	ChiakiErrorCode e = chiaki_regist_start(&r->regist, &r->log, &info, regist_cb, r);
	*err = (int)e;
	if(e != CHIAKI_ERR_SUCCESS)
	{
		free(r);
		return NULL;
	}
	return r;
}

void vit_regist_free(void *regist)
{
	Regist *r = regist;
	if(!r)
		return;
	chiaki_regist_stop(&r->regist);
	chiaki_regist_fini(&r->regist);
	free(r);
}

// ---------------------------------------------------------------------------- playing

typedef struct
{
	ChiakiLog log;
	LogTarget log_target;
	ChiakiSession session;
	VitPsCallbacks cb;
	ChiakiControllerState controller;
} Session;

static void event_cb(ChiakiEvent *event, void *user)
{
	Session *s = user;
	if(!s->cb.event)
		return;
	switch(event->type)
	{
		case CHIAKI_EVENT_CONNECTED:
			s->cb.event(VIT_EVENT_CONNECTED, 0, NULL, s->cb.user);
			break;
		case CHIAKI_EVENT_LOGIN_PIN_REQUEST:
			s->cb.event(VIT_EVENT_LOGIN_PIN, event->login_pin_request.pin_incorrect ? 1 : 0, NULL, s->cb.user);
			break;
		case CHIAKI_EVENT_QUIT:
		{
			const char *why = event->quit.reason_str ? event->quit.reason_str : chiaki_quit_reason_string(event->quit.reason);
			s->cb.event(VIT_EVENT_QUIT, chiaki_quit_reason_is_error(event->quit.reason) ? 1 : 0, why, s->cb.user);
			break;
		}
		case CHIAKI_EVENT_RUMBLE:
			s->cb.event(VIT_EVENT_RUMBLE, (event->rumble.left << 8) | event->rumble.right, NULL, s->cb.user);
			break;
		case CHIAKI_EVENT_NICKNAME_RECEIVED:
			s->cb.event(VIT_EVENT_NICKNAME, 0, event->server_nickname, s->cb.user);
			break;
		default:
			break;
	}
}

static bool video_cb(uint8_t *buf, size_t buf_size, int32_t frames_lost, bool frame_recovered, void *user)
{
	Session *s = user;
	// (on chiaki's own thread: the frame being handed over is the receiver's current one)
	ChiakiVideoReceiver *vr = s->session.stream_connection.video_receiver;
	int index = vr ? (int)vr->frame_index_cur : -1;
	return s->cb.video ? s->cb.video(buf, buf_size, (int)frames_lost, frame_recovered, index, s->cb.user) : true;
}

static void audio_header_cb(ChiakiAudioHeader *header, void *user)
{
	Session *s = user;
	if(s->cb.audio_header)
		s->cb.audio_header(header->channels, (int)header->rate, (int)header->frame_size, s->cb.user);
}

static void audio_frame_cb(uint8_t *buf, size_t buf_size, void *user)
{
	Session *s = user;
	if(s->cb.audio_frame)
		s->cb.audio_frame(buf, buf_size, s->cb.user);
}

void *vit_session_start(const char *host, const char regist_key[16], const uint8_t rp_key[16], unsigned width, unsigned height, unsigned fps,
		unsigned bitrate, int codec, const VitPsCallbacks *cb, int *err)
{
	Session *s = calloc(1, sizeof(Session));
	if(!s)
	{
		*err = CHIAKI_ERR_MEMORY;
		return NULL;
	}
	s->cb = *cb;
	log_init(&s->log, &s->log_target, cb->log, cb->user);
	chiaki_controller_state_set_idle(&s->controller);

	ChiakiConnectInfo info = { 0 };
	info.ps5 = true;
	info.host = host;
	memcpy(info.regist_key, regist_key, sizeof(info.regist_key));
	memcpy(info.morning, rp_key, sizeof(info.morning));
	info.video_profile.width = width;
	info.video_profile.height = height;
	info.video_profile.max_fps = fps;
	info.video_profile.bitrate = bitrate;
	info.video_profile.codec = codec == 1 ? CHIAKI_CODEC_H265 : CHIAKI_CODEC_H264;
	info.video_profile_auto_downgrade = true;
	info.enable_keyboard = false;
	info.enable_dualsense = false;
	info.packet_loss_max = 0.05;
	info.enable_idr_on_fec_failure = true;

	ChiakiErrorCode e = chiaki_session_init(&s->session, &info, &s->log);
	if(e != CHIAKI_ERR_SUCCESS)
	{
		*err = (int)e;
		free(s);
		return NULL;
	}
	chiaki_session_set_event_cb(&s->session, event_cb, s);
	chiaki_session_set_video_sample_cb(&s->session, video_cb, s);
	ChiakiAudioSink sink = { 0 };
	sink.user = s;
	sink.header_cb = audio_header_cb;
	sink.frame_cb = audio_frame_cb;
	chiaki_session_set_audio_sink(&s->session, &sink);

	e = chiaki_session_start(&s->session);
	*err = (int)e;
	if(e != CHIAKI_ERR_SUCCESS)
	{
		chiaki_session_fini(&s->session);
		free(s);
		return NULL;
	}
	return s;
}

void vit_session_controller(void *session, uint32_t buttons, uint8_t l2, uint8_t r2, int16_t lx, int16_t ly, int16_t rx, int16_t ry)
{
	Session *s = session;
	if(!s)
		return;
	s->controller.buttons = buttons;
	s->controller.l2_state = l2;
	s->controller.r2_state = r2;
	s->controller.left_x = lx;
	s->controller.left_y = ly;
	s->controller.right_x = rx;
	s->controller.right_y = ry;
	chiaki_session_set_controller_state(&s->session, &s->controller);
}

void vit_session_login_pin(void *session, const char *pin)
{
	Session *s = session;
	if(s && pin)
		chiaki_session_set_login_pin(&s->session, (const uint8_t *)pin, strlen(pin));
}

void vit_session_request_idr(void *session)
{
	Session *s = session;
	if(s)
		chiaki_session_request_idr(&s->session);
}

void vit_session_report_corrupt(void *session, int start, int end)
{
	Session *s = session;
	if(s && start >= 0 && end >= start)
		stream_connection_send_corrupt_frame(&s->session.stream_connection, (ChiakiSeqNum16)start, (ChiakiSeqNum16)end);
}

void vit_session_free(void *session)
{
	Session *s = session;
	if(!s)
		return;
	chiaki_session_stop(&s->session);
	chiaki_session_join(&s->session);
	chiaki_session_fini(&s->session);
	free(s);
}
