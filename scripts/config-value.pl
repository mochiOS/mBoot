#!/usr/bin/env perl

use strict;
use warnings;
use FindBin;
use lib $FindBin::Bin;
use MbootConfig qw(read_mboot_config);

@ARGV == 2 or die "usage: $0 CONFIG KEY\n";
my ($file, $key) = @ARGV;
my $config = read_mboot_config($file);
if ($key eq 'domain_count') {
    print scalar(@{$config->{domains}}), "\n";
    exit 0;
}
if ($key eq 'system_image') {
    my ($system) = grep { $_->{role} eq 'system' } @{$config->{domains}};
    print "$system->{image}\n";
    exit 0;
}
if ($key eq 'hardware_bootstrap_id') {
    my @domains = grep {
        $_->{role} eq 'hardware' && $_->{image} eq 'hardware-bootstrap'
    } @{$config->{domains}};
    @domains <= 1 or die "multiple hardware-bootstrap Domains are unsupported\n";
    print(@domains ? $domains[0]->{id} : 0, "\n");
    exit 0;
}
exists $config->{$key} && ref($config->{$key}) eq ''
    or die "unknown scalar config key: $key\n";
print "$config->{$key}\n";
