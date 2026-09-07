/* Core commands: type conversions (slot ABI). Text results via the channel. */
#include <stdio.h>
#include <stdlib.h>
#include <string.h>
#include "kiln_core.h"

static char *dup_buf(const char *tmp){
    long n=(long)strlen(tmp)+1; char *out=(char*)kn_malloc(n); memcpy(out,tmp,n); return out;
}

void kn_int_to_double(Kiln_Slot *r, int32_t c, Kiln_Slot *argv){ (void)c; kn_ret_double(r,(double)kn_arg_int(argv,0)); }
void kn_double_to_int(Kiln_Slot *r, int32_t c, Kiln_Slot *argv){ (void)c; kn_ret_int(r,(int)kn_arg_double(argv,0)); }
void kn_int_to_int64(Kiln_Slot *r, int32_t c, Kiln_Slot *argv){ (void)c; kn_ret_int64(r,(long long)kn_arg_int(argv,0)); }
void kn_int64_to_int(Kiln_Slot *r, int32_t c, Kiln_Slot *argv){ (void)c; kn_ret_int(r,(int)kn_arg_int64(argv,0)); }

void kn_text_to_int(Kiln_Slot *r, int32_t c, Kiln_Slot *argv){ (void)c; const char*s=kn_arg_text(argv,0); kn_ret_int(r, s?atoi(s):0); }
void kn_text_to_double(Kiln_Slot *r, int32_t c, Kiln_Slot *argv){ (void)c; const char*s=kn_arg_text(argv,0); kn_ret_double(r, s?atof(s):0.0); }

void kn_int_to_text(Kiln_Slot *r, int32_t c, Kiln_Slot *argv){ (void)c; char t[32]; snprintf(t,sizeof t,"%d",kn_arg_int(argv,0)); kn_ret_text(r,dup_buf(t)); }
void kn_int64_to_text(Kiln_Slot *r, int32_t c, Kiln_Slot *argv){ (void)c; char t[32]; snprintf(t,sizeof t,"%lld",(long long)kn_arg_int64(argv,0)); kn_ret_text(r,dup_buf(t)); }
void kn_double_to_text(Kiln_Slot *r, int32_t c, Kiln_Slot *argv){ (void)c; char t[64]; snprintf(t,sizeof t,"%g",kn_arg_double(argv,0)); kn_ret_text(r,dup_buf(t)); }
