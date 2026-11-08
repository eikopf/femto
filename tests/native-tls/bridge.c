/* Host ABI bridge for the actual device Rust TLS owner. */
#include <mbedtls/ssl.h>
#include <mbedtls/entropy.h>
#include <mbedtls/ctr_drbg.h>
#include <mbedtls/x509_crt.h>
#include <stdlib.h>
#include <string.h>
#include <stdio.h>
typedef struct { void *p; } holder;
typedef struct { mbedtls_ssl_config config; mbedtls_x509_crt ca; } configuration;
static _Thread_local char ca_path[4096];
void femto_set_ca(const char *path) { snprintf(ca_path, sizeof(ca_path), "%s", path); }
#define INIT(name, type, init) void femto_##name(holder *h) { h->p = calloc(1, sizeof(type)); if (!h->p) abort(); init(h->p); }
#define FREE(name, freefn) void femto_##name(holder *h) { freefn(h->p); free(h->p); h->p = NULL; }
INIT(ssl_init, mbedtls_ssl_context, mbedtls_ssl_init)
INIT(entropy_init, mbedtls_entropy_context, mbedtls_entropy_init)
INIT(ctr_drbg_init, mbedtls_ctr_drbg_context, mbedtls_ctr_drbg_init)
FREE(ssl_free, mbedtls_ssl_free)
FREE(entropy_free, mbedtls_entropy_free)
FREE(ctr_drbg_free, mbedtls_ctr_drbg_free)
void femto_ssl_config_init(holder *h) {
    configuration *c = calloc(1, sizeof(*c)); if (!c) abort();
    mbedtls_ssl_config_init(&c->config); mbedtls_x509_crt_init(&c->ca); h->p = c;
}
void femto_ssl_config_free(holder *h) {
    configuration *c = h->p; mbedtls_ssl_config_free(&c->config); mbedtls_x509_crt_free(&c->ca); free(c); h->p = NULL;
}
int femto_entropy_func(void *h, unsigned char *b, size_t n) { return mbedtls_entropy_func(((holder *)h)->p, b, n); }
int femto_ctr_drbg_random(void *h, unsigned char *b, size_t n) { return mbedtls_ctr_drbg_random(((holder *)h)->p, b, n); }
int femto_ctr_drbg_seed(holder *h, int (*rng)(void *, unsigned char *, size_t), void *ctx, const unsigned char *custom, size_t n) {
    return mbedtls_ctr_drbg_seed(h->p, rng, ctx, custom, n);
}
int femto_ssl_config_defaults(holder *h, int endpoint, int transport, int preset) { return mbedtls_ssl_config_defaults(&((configuration *)h->p)->config, endpoint, transport, preset); }
void femto_ssl_conf_rng(holder *h, int (*rng)(void *, unsigned char *, size_t), void *ctx) { mbedtls_ssl_conf_rng(&((configuration *)h->p)->config, rng, ctx); }
void femto_ssl_conf_authmode(holder *h, int mode) { mbedtls_ssl_conf_authmode(&((configuration *)h->p)->config, mode); }
void femto_ssl_conf_session_tickets(holder *h, int mode) { mbedtls_ssl_conf_session_tickets(&((configuration *)h->p)->config, mode); }
void femto_ssl_conf_renegotiation(holder *h, int mode) { mbedtls_ssl_conf_renegotiation(&((configuration *)h->p)->config, mode); }
int femto_crt_bundle_attach(void *h) {
    configuration *c = ((holder *)h)->p;
    int result = mbedtls_x509_crt_parse_file(&c->ca, ca_path);
    if (result != 0) return result;
    mbedtls_ssl_conf_ca_chain(&c->config, &c->ca, NULL); return 0;
}
int femto_ssl_setup(holder *h, const holder *config) { return mbedtls_ssl_setup(h->p, &((configuration *)config->p)->config); }
int femto_ssl_set_hostname(holder *h, const char *name) { return mbedtls_ssl_set_hostname(h->p, name); }
void femto_ssl_set_bio(holder *h, void *ctx, mbedtls_ssl_send_t *send, mbedtls_ssl_recv_t *recv, mbedtls_ssl_recv_timeout_t *timeout) { mbedtls_ssl_set_bio(h->p, ctx, send, recv, timeout); }
int femto_ssl_handshake(holder *h) { return mbedtls_ssl_handshake(h->p); }
unsigned int femto_ssl_get_verify_result(const holder *h) { return mbedtls_ssl_get_verify_result(h->p); }
int femto_ssl_read(holder *h, unsigned char *b, size_t n) { return mbedtls_ssl_read(h->p, b, n); }
int femto_ssl_write(holder *h, const unsigned char *b, size_t n) { return mbedtls_ssl_write(h->p, b, n); }
