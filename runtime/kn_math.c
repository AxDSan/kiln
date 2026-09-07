/* Core commands: math (slot ABI). */
#include <math.h>
#include "kiln_core.h"

#define A_I(i) kn_arg_int(argv, i)
#define A_D(i) kn_arg_double(argv, i)

void kn_abs_int(Kiln_Slot *r, int32_t c, Kiln_Slot *argv){ (void)c; int a=A_I(0); kn_ret_int(r, a<0?-a:a); }
void kn_min_int(Kiln_Slot *r, int32_t c, Kiln_Slot *argv){ (void)c; int a=A_I(0),b=A_I(1); kn_ret_int(r, a<b?a:b); }
void kn_max_int(Kiln_Slot *r, int32_t c, Kiln_Slot *argv){ (void)c; int a=A_I(0),b=A_I(1); kn_ret_int(r, a>b?a:b); }
void kn_mod_int(Kiln_Slot *r, int32_t c, Kiln_Slot *argv){ (void)c; int a=A_I(0),b=A_I(1); kn_ret_int(r, b==0?0:a%b); }
void kn_pow_int(Kiln_Slot *r, int32_t c, Kiln_Slot *argv){
    (void)c; int base=A_I(0), e=A_I(1), out=1;
    if (e<0){ kn_ret_int(r,0); return; }
    for (int i=0;i<e;i++) out*=base;
    kn_ret_int(r,out);
}

void kn_sqrt(Kiln_Slot *r, int32_t c, Kiln_Slot *argv){ (void)c; kn_ret_double(r, sqrt(A_D(0))); }
void kn_sin(Kiln_Slot *r, int32_t c, Kiln_Slot *argv){ (void)c; kn_ret_double(r, sin(A_D(0))); }
void kn_cos(Kiln_Slot *r, int32_t c, Kiln_Slot *argv){ (void)c; kn_ret_double(r, cos(A_D(0))); }
void kn_tan(Kiln_Slot *r, int32_t c, Kiln_Slot *argv){ (void)c; kn_ret_double(r, tan(A_D(0))); }
void kn_pow(Kiln_Slot *r, int32_t c, Kiln_Slot *argv){ (void)c; kn_ret_double(r, pow(A_D(0),A_D(1))); }
void kn_exp(Kiln_Slot *r, int32_t c, Kiln_Slot *argv){ (void)c; kn_ret_double(r, exp(A_D(0))); }
void kn_ln(Kiln_Slot *r, int32_t c, Kiln_Slot *argv){ (void)c; kn_ret_double(r, log(A_D(0))); }
void kn_log10(Kiln_Slot *r, int32_t c, Kiln_Slot *argv){ (void)c; kn_ret_double(r, log10(A_D(0))); }
void kn_floor(Kiln_Slot *r, int32_t c, Kiln_Slot *argv){ (void)c; kn_ret_double(r, floor(A_D(0))); }
void kn_ceil(Kiln_Slot *r, int32_t c, Kiln_Slot *argv){ (void)c; kn_ret_double(r, ceil(A_D(0))); }
void kn_round(Kiln_Slot *r, int32_t c, Kiln_Slot *argv){ (void)c; kn_ret_double(r, round(A_D(0))); }
void kn_abs_double(Kiln_Slot *r, int32_t c, Kiln_Slot *argv){ (void)c; kn_ret_double(r, fabs(A_D(0))); }
void kn_min_double(Kiln_Slot *r, int32_t c, Kiln_Slot *argv){ (void)c; double a=A_D(0),b=A_D(1); kn_ret_double(r, a<b?a:b); }
void kn_max_double(Kiln_Slot *r, int32_t c, Kiln_Slot *argv){ (void)c; double a=A_D(0),b=A_D(1); kn_ret_double(r, a>b?a:b); }
