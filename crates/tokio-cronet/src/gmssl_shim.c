#include <gmssl/tls.h>
#include <gmssl/sm3.h>
#include <gmssl/x509.h>
#include <gmssl/version.h>
#include <stdint.h>
#include <stdio.h>
#include <stdlib.h>
#include <string.h>

#if GMSSL_VERSION_NUM != 30200
#error "tokio-cronet's gmssl feature requires GmSSL 3.2.0"
#endif

#if defined(_WIN32)
#define CRONET_GMSSL_EXPORT __declspec(dllexport)
#else
#define CRONET_GMSSL_EXPORT __attribute__((visibility("default")))
#endif

typedef struct {
    TLS_CTX context;
    TLS_CONNECT connection;
    int context_initialized;
    int connection_initialized;
    int protocol;
} cronet_gmssl_client;

static void cronet_gmssl_set_error(char *output, size_t capacity,
                                   const char *message) {
    if (output == NULL || capacity == 0) {
        return;
    }
    snprintf(output, capacity, "%s", message);
}

static void cronet_gmssl_client_free(cronet_gmssl_client *client) {
    if (client == NULL) {
        return;
    }
    if (client->connection_initialized) {
        tls_cleanup(&client->connection);
    }
    if (client->context_initialized) {
        tls_ctx_cleanup(&client->context);
    }
    free(client);
}

CRONET_GMSSL_EXPORT cronet_gmssl_client *cronet_gmssl_client_connect(
    int protocol, intptr_t socket_value, const char *ca_certificates,
    int verify_depth, const char *client_certificates,
    const char *client_private_key, const char *client_key_password,
    char *error_output, size_t error_capacity) {
    cronet_gmssl_client *client;
    int cipher_suite;
    int supported_group = TLS_curve_sm2p256v1;
    int signature_algorithm = TLS_sig_sm2sig_sm3;

    if (ca_certificates == NULL) {
        cronet_gmssl_set_error(error_output, error_capacity,
                               "CA certificate path is required");
        return NULL;
    }
    switch (protocol) {
    case TLS_protocol_tlcp:
        cipher_suite = TLS_cipher_ecc_sm4_cbc_sm3;
        break;
    case TLS_protocol_tls12:
        cipher_suite = TLS_cipher_ecdhe_sm4_cbc_sm3;
        break;
    case TLS_protocol_tls13:
        cipher_suite = TLS_cipher_sm4_gcm_sm3;
        break;
    default:
        cronet_gmssl_set_error(error_output, error_capacity,
                               "unsupported GmSSL protocol");
        return NULL;
    }

    client = (cronet_gmssl_client *)calloc(1, sizeof(*client));
    if (client == NULL) {
        cronet_gmssl_set_error(error_output, error_capacity,
                               "failed to allocate GmSSL client");
        return NULL;
    }
    client->protocol = protocol;

    if (tls_ctx_init(&client->context, protocol, TLS_client_mode) != 1) {
        cronet_gmssl_set_error(error_output, error_capacity,
                               "tls_ctx_init failed");
        cronet_gmssl_client_free(client);
        return NULL;
    }
    client->context_initialized = 1;
    if (tls_ctx_set_cipher_suites(&client->context, &cipher_suite, 1) != 1) {
        cronet_gmssl_set_error(error_output, error_capacity,
                               "tls_ctx_set_cipher_suites failed");
        cronet_gmssl_client_free(client);
        return NULL;
    }
    if (tls_ctx_set_supported_groups(&client->context, &supported_group, 1) !=
        1) {
        cronet_gmssl_set_error(error_output, error_capacity,
                               "tls_ctx_set_supported_groups failed");
        cronet_gmssl_client_free(client);
        return NULL;
    }
    if (tls_ctx_set_signature_algorithms(&client->context,
                                         &signature_algorithm, 1) != 1) {
        cronet_gmssl_set_error(error_output, error_capacity,
                               "tls_ctx_set_signature_algorithms failed");
        cronet_gmssl_client_free(client);
        return NULL;
    }
    if (tls_ctx_set_ca_certificates(&client->context, ca_certificates,
                                    verify_depth) != 1) {
        cronet_gmssl_set_error(error_output, error_capacity,
                               "tls_ctx_set_ca_certificates failed");
        cronet_gmssl_client_free(client);
        return NULL;
    }
    if (client_certificates != NULL &&
        tls_ctx_set_certificate_and_key(
            &client->context, client_certificates, client_private_key,
            client_key_password) != 1) {
        cronet_gmssl_set_error(error_output, error_capacity,
                               "tls_ctx_set_certificate_and_key failed");
        cronet_gmssl_client_free(client);
        return NULL;
    }
    if (tls_init(&client->connection, &client->context) != 1) {
        cronet_gmssl_set_error(error_output, error_capacity, "tls_init failed");
        cronet_gmssl_client_free(client);
        return NULL;
    }
    client->connection_initialized = 1;
    if (tls_set_socket(&client->connection, (tls_socket_t)socket_value) != 1) {
        cronet_gmssl_set_error(error_output, error_capacity,
                               "tls_set_socket failed");
        cronet_gmssl_client_free(client);
        return NULL;
    }
    if (tls_do_handshake(&client->connection) != 1) {
        cronet_gmssl_set_error(error_output, error_capacity,
                               "GmSSL handshake failed");
        cronet_gmssl_client_free(client);
        return NULL;
    }
    return client;
}

CRONET_GMSSL_EXPORT int cronet_gmssl_client_peer_leaf_sm3(
    cronet_gmssl_client *client, uint8_t output[32]) {
    const uint8_t *certificate = NULL;
    size_t certificate_length = 0;
    SM3_CTX sm3_ctx;

    if (client == NULL || output == NULL) {
        return -1;
    }
    if (x509_certs_get_cert_by_index(client->connection.peer_cert_chain,
                                     client->connection.peer_cert_chain_len, 0,
                                     &certificate,
                                     &certificate_length) != 1) {
        return -1;
    }
    sm3_init(&sm3_ctx);
    sm3_update(&sm3_ctx, certificate, certificate_length);
    sm3_finish(&sm3_ctx, output);
    return 1;
}

CRONET_GMSSL_EXPORT int cronet_gmssl_client_send(
    cronet_gmssl_client *client, const uint8_t *input, size_t input_length,
    size_t *sent_length) {
    if (client == NULL) {
        return -1;
    }
    if (client->protocol == TLS_protocol_tls13) {
        return tls13_send(&client->connection, input, input_length, sent_length);
    }
    return tls_send(&client->connection, input, input_length, sent_length);
}

CRONET_GMSSL_EXPORT int cronet_gmssl_client_recv(
    cronet_gmssl_client *client, uint8_t *output, size_t output_capacity,
    size_t *received_length) {
    if (client == NULL) {
        return -1;
    }
    if (client->protocol == TLS_protocol_tls13) {
        return tls13_recv(&client->connection, output, output_capacity,
                          received_length);
    }
    return tls_recv(&client->connection, output, output_capacity,
                    received_length);
}

CRONET_GMSSL_EXPORT void
cronet_gmssl_client_destroy(cronet_gmssl_client *client) {
    cronet_gmssl_client_free(client);
}
