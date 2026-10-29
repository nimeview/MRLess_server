#ifndef MRLESS_H
#define MRLESS_H

#include <stdint.h>

#ifdef __cplusplus
extern "C" {
#endif

/** Start the mesh node. data_dir is required; name may be NULL. Returns 0 on success. */
int32_t mrless_start(const char *data_dir, const char *name);

void mrless_stop(void);

/** Local UI WebSocket port, or 0 if the node is not running. */
uint16_t mrless_ws_port(void);

/** Mesh TCP port peers dial, or 0 if the node is not running. */
uint16_t mrless_mesh_port(void);

/** Ask the node to dial host:port (e.g. from Bonjour). Returns 0 on accept. */
int32_t mrless_connect_peer(const char *host_port);

/** Re-dial remembered peers and flush outbox. Returns 0 on accept. */
int32_t mrless_reconnect_peers(void);

/** Heap UUID string for this node; free with mrless_string_free. */
char *mrless_node_id(void);

char *mrless_version(void);
void mrless_string_free(char *p);

#ifdef __cplusplus
}
#endif

#endif
