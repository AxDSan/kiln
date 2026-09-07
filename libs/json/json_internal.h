/* Internal shape of the `json` library: the document tree, the parser, and the
 * serializer.  Nothing here crosses the ABI — a program only ever sees a
 * handle, a path string, and one of the five Kiln types.
 *
 * Tree memory is plain malloc/free, not kn_malloc: it is library bookkeeping
 * with a lifetime the handle table controls, not program data the runtime frees
 * at exit.  Only text HANDED BACK to a program goes through kn_malloc.
 */
#ifndef KILN_JSON_INTERNAL_H
#define KILN_JSON_INTERNAL_H

#include <stddef.h>

enum {
    JSON_T_NULL = 0,
    JSON_T_BOOL = 1,
    JSON_T_NUM  = 2,
    JSON_T_STR  = 3,
    JSON_T_ARR  = 4,
    JSON_T_OBJ  = 5
};

typedef struct JsonNode JsonNode;
struct JsonNode {
    int        type;
    int        bval;      /* JSON_T_BOOL                                   */
    double     num;       /* JSON_T_NUM                                    */
    char      *str;       /* JSON_T_STR, NUL-terminated UTF-8              */
    JsonNode **kids;      /* JSON_T_ARR / JSON_T_OBJ elements              */
    char     **keys;      /* JSON_T_OBJ member names, parallel to kids     */
    long       count;
    long       cap;
};

JsonNode *json_node_new(int type);
void      json_node_free(JsonNode *v);
void      json_node_free_payload(void *p);          /* handle-table close fn */

int       json_node_set_str(JsonNode *v, const char *s);  /* 1 ok, 0 no memory */
int       json_arr_push(JsonNode *a, JsonNode *child);
int       json_obj_put(JsonNode *o, const char *key, JsonNode *child);
long      json_obj_index(const JsonNode *o, const char *key);  /* -1 if absent */
void      json_container_remove(JsonNode *c, long i);

/* Parse a complete document.  Returns NULL on malformed input and writes a
 * one-line reason — including the byte offset — into `errbuf`. */
JsonNode *json_parse_text(const char *src, char *errbuf, size_t errcap);

/* Serialize to a plain malloc'd C string (caller frees).  NULL on no memory. */
char     *json_write(const JsonNode *v, int pretty);

#endif /* KILN_JSON_INTERNAL_H */
